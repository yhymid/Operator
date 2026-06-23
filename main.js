require('dotenv').config({ quiet: true });

const {
  Client,
  GatewayIntentBits,
  PermissionFlagsBits,
  ActionRowBuilder,
  ModalBuilder,
  TextInputBuilder,
  TextInputStyle,
} = require('discord.js');

const AutoClean = require('./src/events/autoclean');
const AutoMod = require('./src/events/automod');
const FullLogs = require('./src/events/full-logs');
const { buildHelpPanelPayload, executeHelpComponentInteraction } = require('./src/cmds/help');
const { executeHelpCommand } = require('./src/cmds/help-command');
const { executeCleanCommand } = require('./src/cmds/clean');
const { executeCacheCommand } = require('./src/cmds/cache');
const { executeMusicButtonInteraction, executeMusicCommand } = require('./src/cmds/music');
const { executeVerifyCommand } = require('./src/cmds/verify');
const { executeAutomodCommand } = require('./src/cmds/automod');
const { executeSettingsCommand } = require('./src/cmds/settings');
const { executeAnnounceAutocomplete, executeAnnounceCommand } = require('./src/cmds/announce');
const { executeLogsCommand } = require('./src/cmds/logs');
const { executeDebugCommand, executeHealthCommand } = require('./src/cmds/diagnostics');
const { executeDmCommand } = require('./src/cmds/dm');
const {
  formatCommandSyncReport,
  syncGuildCommands,
} = require('./src/core/command-sync');
const { MusicManager } = require('./src/core/music');
const { runAnnouncementSweep } = require('./src/core/announcements');
const { formatConfigError, readOperatorConfigSync } = require('./src/core/config');
const { resolveOwnerIds } = require('./src/core/owners');
const { resolveOperatorPaths } = require('./src/core/paths');
const { initServerStateStore } = require('./src/core/state');
const { addOneCalendarMonthIso, generateVerificationCode } = require('./src/core/verification');

const logger = {
  info: (msg) => console.log(`[INFO] ${msg}`),
  warn: (msg) => console.warn(`[WARN] ${msg}`),
  error: (msg) => console.error(`[ERROR] ${msg}`),
  success: (msg) => console.log(`[SUCCESS] ${msg}`),
};
const OPERATOR_PATHS = resolveOperatorPaths();
const CONFIG_PATH = OPERATOR_PATHS.configPath;
let config;
try {
  config = readOperatorConfigSync(CONFIG_PATH);
} catch (error) {
  logger.error(formatConfigError(error, { prefix: 'Startup aborted. ' }));
  process.exit(1);
}
const STARTUP_VERBOSE = config.startupVerboseLogs === true;
const SERVER_CACHE_DIR = OPERATOR_PATHS.serverCacheDir;
const NODE_MAJOR = Number(process.versions.node.split('.')[0]);
let isShuttingDown = false;
let verificationSweepTimer = null;
let announcementSweepTimer = null;
let verificationSweepCount = 0;
const VERIFY_CAPTCHA_MODAL_ID = 'verify_captcha_modal_v1';
const VERIFY_CAPTCHA_INPUT_ID = 'verify_captcha_input_v1';
const VERIFY_CAPTCHA_TTL_MS = 5 * 60 * 1000;
const VERIFY_LOCKOUT_MS = 60 * 60 * 1000;
const VERIFICATION_SWEEP_INTERVAL_MS = 10 * 60 * 1000;
const VERIFICATION_FULL_SWEEP_EVERY = 72;
const serverState = initServerStateStore({
  cacheDir: SERVER_CACHE_DIR,
  logger,
});

const client = new Client({
  intents: [
    GatewayIntentBits.Guilds,
    GatewayIntentBits.GuildMessages,
    GatewayIntentBits.MessageContent,
    GatewayIntentBits.GuildMembers,
    GatewayIntentBits.GuildVoiceStates,
    GatewayIntentBits.GuildModeration,
    GatewayIntentBits.GuildMessageReactions,
    GatewayIntentBits.GuildEmojisAndStickers,
    GatewayIntentBits.GuildInvites,
    GatewayIntentBits.GuildWebhooks,
    GatewayIntentBits.AutoModerationConfiguration,
    GatewayIntentBits.AutoModerationExecution,
  ],
});

const cleaner = new AutoClean(client, {
  logger,
  configPath: CONFIG_PATH,
  stateStore: serverState,
});
const automod = new AutoMod(client, {
  logger,
  configPath: CONFIG_PATH,
});
const fullLogs = new FullLogs(client, {
  logger,
  stateStore: serverState,
  logsDir: OPERATOR_PATHS.serverLogsDir,
});
const musicManager = new MusicManager(client, {
  logger,
  queueStateDir: OPERATOR_PATHS.musicQueueDir,
});

client.once('clientReady', async () => {
  if (Number.isFinite(NODE_MAJOR) && NODE_MAJOR !== 22) {
    logger.warn(`Wykryto Node.js ${process.versions.node}. Dla voice docelowy runtime to Node 22 LTS.`);
  }
  logger.success(`Bot zalogowany jako ${client.user.tag}`);
  if (STARTUP_VERBOSE) {
    logger.info(`Serwery: ${client.guilds.cache.size}`);
  }
  const ownerAccess = resolveOwnerIds({ configPath: CONFIG_PATH });
  if (ownerAccess.ids.size === 0) {
    logger.warn('Brak OWNER_IDS w env ani ownerIds w config.json. Komendy owner-only beda niedostepne.');
  } else if (STARTUP_VERBOSE) {
    logger.info(`Owner access loaded from ${ownerAccess.source}. IDs: ${ownerAccess.ids.size}`);
  }
  if (ownerAccess.invalid.length > 0) {
    logger.warn(`Pominieto niepoprawne OWNER_IDS: ${ownerAccess.invalid.join(', ')}`);
  }
  serverState.migrateLegacyServerCache();
  markAllGuildsOnline('startup', { silent: !STARTUP_VERBOSE });

  await registerSlashCommands({ verbose: STARTUP_VERBOSE });
  cleaner.start();
  automod.start();
  fullLogs.start();
  await musicManager.initializeProviders();
  cleanupVerificationChallengesOnStartup();
  startVerificationSweepRunner();
  await runVerificationSweep('startup', { fullScan: true });
  await runAnnouncementSweep({ client, stateStore: serverState, logger });
  startAnnouncementSweepRunner();

  logger.info(`Start zakonczony. Serwery: ${client.guilds.cache.size}. Nastepne czyszczenie: ${cleaner.getNextRunText()}`);
});

client.on('guildCreate', (guild) => {
  serverState.markGuildOnline(guild, 'guildCreate');
});

client.on('guildDelete', (guild) => {
  musicManager.stop(guild.id).catch((error) => {
    logger.warn(`Music stop on guildDelete failed guild=${guild.id}: ${error.message}`);
  });
  serverState.markGuildOffline(
    {
      id: guild.id,
      name: guild.name,
    },
    'guildDelete',
  );
});

client.on('messageCreate', async (message) => {
  if (message.author.bot) return;

  try {
    const consumedByAutomod = await automod.handleMessage(message);
    if (consumedByAutomod) {
      return;
    }
  } catch (error) {
    logger.error(`Automod error: ${error.message}`);
  }
});

client.on('error', (error) => {
  logger.error(`Discord error: ${error.message}`);
});

client.on('interactionCreate', async (interaction) => {
  if (interaction.isButton() || interaction.isStringSelectMenu()) {
    try {
      const consumedByHelp = await executeHelpComponentInteraction(interaction);
      if (consumedByHelp) {
        return;
      }

      const consumedByMusic = await executeMusicButtonInteraction(interaction, {
        musicManager,
        logger,
      });
      if (consumedByMusic) {
        return;
      }

      if (interaction.isButton()) {
        await handleVerifyButtonInteraction(interaction);
      }
    } catch (error) {
      logger.error(`Component interaction error: ${error.message}`);
      if (!interaction.replied && !interaction.deferred) {
        await interaction.reply({
          content: 'Wystapil blad podczas obslugi panelu.',
          ephemeral: true,
        }).catch(() => {});
      }
    }
    return;
  }

  if (interaction.isModalSubmit()) {
    try {
      await handleVerifyCaptchaModal(interaction);
    } catch (error) {
      logger.error(`Verify captcha modal error: ${error.message}`);
      if (!interaction.replied && !interaction.deferred) {
        await interaction.reply({
          content: 'Wystapil blad podczas sprawdzania captcha.',
          ephemeral: true,
        }).catch(() => {});
      }
    }
    return;
  }

  if (interaction.isAutocomplete()) {
    try {
      if (interaction.commandName === 'announce') {
        await executeAnnounceAutocomplete(interaction, {
          stateStore: serverState,
        });
      }
    } catch (error) {
      logger.error(`Autocomplete error: ${error.message}`);
      await interaction.respond([]).catch(() => {});
    }
    return;
  }

  if (!interaction.isChatInputCommand()) return;

  try {
    if (interaction.commandName === 'clean') {
      await executeCleanCommand(interaction, { cleaner, logger });
      return;
    }

    if (interaction.commandName === 'help') {
      await executeHelpCommand(interaction, { buildHelpPayload: buildHelpPanelPayload });
      return;
    }

    if (interaction.commandName === 'cache') {
      await executeCacheCommand(interaction, { cacheDir: SERVER_CACHE_DIR, logger });
      return;
    }

    if (interaction.commandName === 'music') {
      await executeMusicCommand(interaction, { musicManager, logger });
      return;
    }

    if (interaction.commandName === 'verify') {
      await executeVerifyCommand(interaction, { stateStore: serverState, logger });
      return;
    }

    if (interaction.commandName === 'automod') {
      await executeAutomodCommand(interaction, { automod });
      return;
    }

    if (interaction.commandName === 'settings') {
      await executeSettingsCommand(interaction, {
        cleaner,
        automod,
        stateStore: serverState,
        configPath: CONFIG_PATH,
      });
      return;
    }

    if (interaction.commandName === 'announce') {
      await executeAnnounceCommand(interaction, {
        stateStore: serverState,
        logger,
      });
      return;
    }

    if (interaction.commandName === 'logs') {
      await executeLogsCommand(interaction, {
        stateStore: serverState,
        fullLogs,
      });
      return;
    }

    if (interaction.commandName === 'health') {
      await executeHealthCommand(interaction, {
        musicManager,
        fullLogs,
        configPath: CONFIG_PATH,
      });
      return;
    }

    if (interaction.commandName === 'debug') {
      await executeDebugCommand(interaction, {
        musicManager,
        fullLogs,
        configPath: CONFIG_PATH,
      });
      return;
    }

    if (interaction.commandName === 'dm') {
      await executeDmCommand(interaction);
      return;
    }
  } catch (error) {
    logger.error(`Interaction error: ${error.message}`);

    if (!interaction.replied && !interaction.deferred) {
      await interaction.reply({
        content: 'Wystapil blad podczas wykonywania komendy.',
        ephemeral: true,
      });
      return;
    }

    await interaction.editReply('Wystapil blad podczas wykonywania komendy.');
  }
});

process.on('unhandledRejection', (error) => {
  logger.error(`Unhandled rejection: ${error.message}`);
});

process.on('SIGINT', () => {
  gracefulShutdown('SIGINT', 0);
});

process.on('SIGTERM', () => {
  gracefulShutdown('SIGTERM', 0);
});

process.on('uncaughtException', (error) => {
  logger.error(`Uncaught exception: ${error.message}`);
  gracefulShutdown('uncaughtException', 1);
});

function gracefulShutdown(reason, exitCode) {
  if (isShuttingDown) return;
  isShuttingDown = true;

  logger.info(`Wylaczanie bota (${reason})...`);
  stopVerificationSweepRunner();
  stopAnnouncementSweepRunner();
  musicManager.shutdown();
  fullLogs.stop();
  markAllGuildsOffline(reason);
  cleaner.stop();
  client.destroy();
  process.exit(exitCode);
}

function markAllGuildsOnline(reason, { silent = false } = {}) {
  const guilds = client.guilds.cache.map((guild) => guild);
  for (const guild of guilds) {
    serverState.markGuildOnline(guild, reason);
  }
  if (!silent) {
    logger.info(`Zapisano status "online" dla ${guilds.length} serwerow.`);
  }
}

function markAllGuildsOffline(reason, { silent = false } = {}) {
  const guilds = client.guilds.cache.map((guild) => guild);
  for (const guild of guilds) {
    serverState.markGuildOffline(guild, reason);
  }
  if (!silent) {
    logger.info(`Zapisano status "offline" dla ${guilds.length} serwerow.`);
  }
}

async function registerSlashCommands({ verbose = false } = {}) {
  try {
    if (!OPERATOR_PATHS.syncCommandsOnStartup) {
      if (verbose) {
        logger.info('Pomijam sync slash commands na starcie (OPERATOR_SYNC_COMMANDS_ON_STARTUP=0).');
      }
      return;
    }

    const report = await syncGuildCommands(client);
    if (report.fail > 0 || verbose) {
      for (const line of formatCommandSyncReport(report)) {
        logger.info(line);
      }
    }
  } catch (error) {
    logger.error(`Nie udalo sie zarejestrowac slash commands: ${error.message}`);
  }
}

function startVerificationSweepRunner() {
  if (verificationSweepTimer) {
    clearInterval(verificationSweepTimer);
  }

  verificationSweepCount = 0;
  verificationSweepTimer = setInterval(() => {
    verificationSweepCount += 1;
    const fullScan = verificationSweepCount % VERIFICATION_FULL_SWEEP_EVERY === 0;
    runVerificationSweep(fullScan ? 'interval-full' : 'interval', { fullScan }).catch((error) => {
      logger.error(`Blad verification sweep: ${error.message}`);
    });
  }, VERIFICATION_SWEEP_INTERVAL_MS);

  verificationSweepTimer.unref?.();
}

function stopVerificationSweepRunner() {
  if (!verificationSweepTimer) return;
  clearInterval(verificationSweepTimer);
  verificationSweepTimer = null;
}

function startAnnouncementSweepRunner() {
  if (announcementSweepTimer) {
    clearInterval(announcementSweepTimer);
  }

  announcementSweepTimer = setInterval(() => {
    runAnnouncementSweep({
      client,
      stateStore: serverState,
      logger,
    }).catch((error) => {
      logger.error(`Blad announcement sweep: ${error.message}`);
    });
  }, 30 * 1000);

  announcementSweepTimer.unref?.();
}

function stopAnnouncementSweepRunner() {
  if (!announcementSweepTimer) return;
  clearInterval(announcementSweepTimer);
  announcementSweepTimer = null;
}

async function handleVerifyButtonInteraction(interaction) {
  if (!interaction.inGuild()) return;

  const guildId = interaction.guildId;
  const verification = serverState.getVerificationState(guildId);
  const panel = verification.panel || {};

  if (!verification.enabled) return;
  if (!panel.customId || interaction.customId !== panel.customId) return;
  if (!panel.messageId || interaction.message.id !== panel.messageId) return;

  const roleId = verification.roleId;
  if (!roleId) {
    await interaction.reply({ content: 'Rola weryfikacyjna nie jest skonfigurowana.', ephemeral: true });
    return;
  }

  const guild = interaction.guild;
  const member = await guild.members.fetch(interaction.user.id).catch(() => null);
  if (!member) {
    await interaction.reply({ content: 'Nie udalo sie pobrac Twoich danych na serwerze.', ephemeral: true });
    return;
  }

  if (member.roles.cache.has(roleId)) {
    await interaction.reply({ content: 'Masz juz aktywna role weryfikacyjna.', ephemeral: true });
    return;
  }

  pruneExpiredVerifyLockouts(guildId);
  const lockoutUntilIso = serverState.getVerificationLockout(guildId, member.id);
  const lockoutUntilTs = Date.parse(lockoutUntilIso || '');
  if (Number.isFinite(lockoutUntilTs) && lockoutUntilTs > Date.now()) {
    await interaction.reply({
      content: `Weryfikacja jest tymczasowo zablokowana. Sprobuj ponownie <t:${Math.floor(lockoutUntilTs / 1000)}:R>.`,
      ephemeral: true,
    });
    return;
  }

  const me = guild.members.me || (await guild.members.fetchMe().catch(() => null));
  const role = guild.roles.cache.get(roleId) || (await guild.roles.fetch(roleId).catch(() => null));

  if (!role) {
    await interaction.reply({ content: 'Skonfigurowana rola weryfikacyjna nie istnieje.', ephemeral: true });
    return;
  }

  if (!me || !me.permissions.has(PermissionFlagsBits.ManageRoles) || role.position >= me.roles.highest.position) {
    await interaction.reply({ content: 'Bot nie moze nadac roli weryfikacyjnej.', ephemeral: true });
    return;
  }

  pruneExpiredVerifyChallenges(guildId);

  const existingChallenge = serverState.getVerificationChallenge(guildId, member.id);
  const existingExpiryTs = Date.parse(existingChallenge?.expiresAt || '');
  const hasActiveChallenge = Boolean(
    existingChallenge
      && Number.isFinite(existingExpiryTs)
      && existingExpiryTs > Date.now()
      && typeof existingChallenge.code === 'string'
      && existingChallenge.code.length > 0,
  );

  const challengeCode = hasActiveChallenge ? existingChallenge.code : generateVerificationCode();

  if (!hasActiveChallenge) {
    const challengeExpiresAtIso = new Date(Date.now() + VERIFY_CAPTCHA_TTL_MS).toISOString();
    serverState.saveVerificationChallenge(guildId, member.id, {
      roleId,
      code: challengeCode,
      attempts: 0,
      expiresAt: challengeExpiresAtIso,
    });
  }

  const modal = new ModalBuilder()
    .setCustomId(VERIFY_CAPTCHA_MODAL_ID)
    .setTitle('Weryfikacja');

  const input = new TextInputBuilder()
    .setCustomId(VERIFY_CAPTCHA_INPUT_ID)
    .setLabel(`Przepisz kod: ${challengeCode}`)
    .setStyle(TextInputStyle.Short)
    .setRequired(true)
    .setMinLength(challengeCode.length)
    .setMaxLength(challengeCode.length);

  modal.addComponents(new ActionRowBuilder().addComponents(input));
  await interaction.showModal(modal);
}

async function handleVerifyCaptchaModal(interaction) {
  if (interaction.customId !== VERIFY_CAPTCHA_MODAL_ID) return;
  if (!interaction.inGuild()) return;

  const guildId = interaction.guildId;
  const userId = interaction.user.id;
  pruneExpiredVerifyLockouts(guildId);
  const lockoutUntilIso = serverState.getVerificationLockout(guildId, userId);
  const lockoutUntilTs = Date.parse(lockoutUntilIso || '');
  if (Number.isFinite(lockoutUntilTs) && lockoutUntilTs > Date.now()) {
    await interaction.reply({
      content: `Weryfikacja jest tymczasowo zablokowana. Sprobuj ponownie <t:${Math.floor(lockoutUntilTs / 1000)}:R>.`,
      ephemeral: true,
    });
    return;
  }

  pruneExpiredVerifyChallenges(guildId);
  const challenge = serverState.getVerificationChallenge(guildId, userId);

  if (!challenge) {
    await interaction.reply({
      content: 'Captcha wygasla. Kliknij przycisk weryfikacji ponownie.',
      ephemeral: true,
    });
    return;
  }

  const challengeExpiryTs = Date.parse(challenge.expiresAt || '');
  if (!Number.isFinite(challengeExpiryTs) || Date.now() > challengeExpiryTs) {
    serverState.removeVerificationChallenge(guildId, userId);
    await interaction.reply({
      content: 'Captcha wygasla. Kliknij przycisk weryfikacji ponownie.',
      ephemeral: true,
    });
    return;
  }

  const answer = interaction.fields
    .getTextInputValue(VERIFY_CAPTCHA_INPUT_ID)
    .trim()
    .toUpperCase();

  if (answer !== challenge.code) {
    const lockoutUntil = new Date(Date.now() + VERIFY_LOCKOUT_MS);
    const lockoutUntilIsoValue = lockoutUntil.toISOString();
    serverState.setVerificationLockout(guildId, userId, lockoutUntilIsoValue);
    serverState.removeVerificationChallenge(guildId, userId);
    await interaction.reply({
      content: `Zly kod. Weryfikacja zablokowana na 1 godzine. Sprobuj ponownie <t:${Math.floor(lockoutUntil.getTime() / 1000)}:R>.`,
      ephemeral: true,
    });
    return;
  }

  const guild = interaction.guild;
  const member = await guild.members.fetch(userId).catch(() => null);
  const me = guild.members.me || (await guild.members.fetchMe().catch(() => null));
  const role = guild.roles.cache.get(challenge.roleId) || (await guild.roles.fetch(challenge.roleId).catch(() => null));

  if (!member || !role || !me) {
    serverState.removeVerificationChallenge(guildId, userId);
    await interaction.reply({
      content: 'Nie mozna dokonczyc weryfikacji w tej chwili.',
      ephemeral: true,
    });
    return;
  }

  if (member.roles.cache.has(role.id)) {
    serverState.removeVerificationChallenge(guildId, userId);
    await interaction.reply({
      content: 'Masz juz aktywna role weryfikacyjna.',
      ephemeral: true,
    });
    return;
  }

  if (!me.permissions.has(PermissionFlagsBits.ManageRoles) || role.position >= me.roles.highest.position) {
    serverState.removeVerificationChallenge(guildId, userId);
    await interaction.reply({
      content: 'Bot nie moze nadac roli weryfikacyjnej.',
      ephemeral: true,
    });
    return;
  }

  try {
    await member.roles.add(role.id, 'Verify button + captcha');
    const expiryIso = addOneCalendarMonthIso(new Date());
    serverState.setUserVerificationExpiry(guildId, member.id, expiryIso);

    serverState.removeVerificationChallenge(guildId, userId);
    await interaction.reply({
      content: `Weryfikacja zakonczona. Rola wygasa <t:${Math.floor(new Date(expiryIso).getTime() / 1000)}:f>.`,
      ephemeral: true,
    });
  } catch (error) {
    logger.error(`Verify captcha assign error: ${error.message}`);
    serverState.removeVerificationChallenge(guildId, userId);
    await interaction.reply({
      content: 'Nie udalo sie nadac roli po sprawdzeniu captcha.',
      ephemeral: true,
    });
  }
}

async function runVerificationSweep(trigger, options = {}) {
  const now = new Date();
  const nowIso = now.toISOString();
  const fullScan = options.fullScan === true;

  for (const guild of client.guilds.cache.values()) {
    const verification = serverState.getVerificationState(guild.id);

    if (!verification?.enabled || !verification.roleId) {
      serverState.saveVerificationState(guild.id, {
        lastSweepAt: nowIso,
        ...(fullScan ? { lastFullSweepAt: nowIso } : {}),
      });
      continue;
    }

    if (verification.challenges && Object.keys(verification.challenges).length > 0) {
      serverState.pruneExpiredVerificationChallenges(guild.id, nowIso);
    }
    if (verification.lockouts && Object.keys(verification.lockouts).length > 0) {
      serverState.pruneExpiredVerificationLockouts(guild.id, nowIso);
    }

    const role = guild.roles.cache.get(verification.roleId) || (await guild.roles.fetch(verification.roleId).catch(() => null));
    if (!role) {
      serverState.saveVerificationState(guild.id, {
        lastSweepAt: nowIso,
        ...(fullScan ? { lastFullSweepAt: nowIso } : {}),
      });
      continue;
    }

    const exceptions = new Set(verification.exceptions || []);
    const expirations = { ...(verification.expirations || {}) };

    if (fullScan) {
      const members = await guild.members.fetch().catch(() => null);

      if (!members) {
        serverState.saveVerificationState(guild.id, {
          lastSweepAt: nowIso,
          lastFullSweepAt: nowIso,
        });
        continue;
      }

      for (const member of members.values()) {
        const hasRole = member.roles.cache.has(role.id);
        const userId = member.id;
        const isException = exceptions.has(userId);

        if (!hasRole) {
          if (expirations[userId]) delete expirations[userId];
          continue;
        }

        if (isException) {
          continue;
        }

        if (!expirations[userId]) {
          expirations[userId] = addOneCalendarMonthIso(now);
          continue;
        }

        const expiryTs = Date.parse(expirations[userId]);
        if (!Number.isFinite(expiryTs)) {
          expirations[userId] = addOneCalendarMonthIso(now);
          continue;
        }

        if (expiryTs <= now.getTime()) {
          try {
            await member.roles.remove(role.id, `Verify expiry (${trigger})`);
          } catch (error) {
            logger.error(`Nie mozna usunac verify na ${guild.id}/${userId}: ${error.message}`);
            continue;
          }
          delete expirations[userId];
        }
      }

      for (const userId of Object.keys(expirations)) {
        if (!members.has(userId)) {
          delete expirations[userId];
        }
      }
    } else {
      for (const userId of Object.keys(expirations)) {
        const member = await guild.members.fetch(userId).catch(() => null);
        if (!member) {
          delete expirations[userId];
          continue;
        }

        if (!member.roles.cache.has(role.id)) {
          delete expirations[userId];
          continue;
        }

        if (exceptions.has(userId)) {
          continue;
        }

        const expiryTs = Date.parse(expirations[userId]);
        if (!Number.isFinite(expiryTs)) {
          expirations[userId] = addOneCalendarMonthIso(now);
          continue;
        }

        if (expiryTs <= now.getTime()) {
          try {
            await member.roles.remove(role.id, `Verify expiry (${trigger})`);
          } catch (error) {
            logger.error(`Nie mozna usunac verify na ${guild.id}/${userId}: ${error.message}`);
            continue;
          }
          delete expirations[userId];
        }
      }
    }

    serverState.saveVerificationState(guild.id, {
      expirations,
      lastSweepAt: nowIso,
      ...(fullScan ? { lastFullSweepAt: nowIso } : {}),
    });
  }
}

function cleanupVerificationChallengesOnStartup() {
  const nowIso = new Date().toISOString();

  for (const guild of client.guilds.cache.values()) {
    const verification = serverState.getVerificationState(guild.id);
    if (!verification?.enabled) continue;
    if (verification.challenges && Object.keys(verification.challenges).length > 0) {
      serverState.pruneExpiredVerificationChallenges(guild.id, nowIso);
    }
    if (verification.lockouts && Object.keys(verification.lockouts).length > 0) {
      serverState.pruneExpiredVerificationLockouts(guild.id, nowIso);
    }
  }
}

function pruneExpiredVerifyChallenges(guildId) {
  if (!guildId) return;
  serverState.pruneExpiredVerificationChallenges(guildId);
}

function pruneExpiredVerifyLockouts(guildId) {
  if (!guildId) return;
  serverState.pruneExpiredVerificationLockouts(guildId);
}

if (!process.env.BOT_TOKEN) {
  logger.error('Brak BOT_TOKEN w .env');
  process.exit(1);
}

client.login(process.env.BOT_TOKEN);
