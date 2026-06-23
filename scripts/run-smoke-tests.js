const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { PermissionsBitField, PermissionFlagsBits } = require('../node_modules/discord.js');

const projectRoot = path.join(__dirname, '..');

function createValidConfig(overrides = {}) {
  return {
    enabled: true,
    cronSchedule: '0 3 * * 0',
    timezone: 'Europe/Warsaw',
    deleteMessagesOlderThanDays: 0,
    deleteLimit: 100,
    deletePinned: false,
    dryRun: false,
    logChannelId: '',
    ownerIds: [],
    notifyBeforeDelete: {
      enabled: false,
      minutesBefore: 10,
      message: 'test',
    },
    whitelist: {
      users: [],
      roles: [],
    },
    channels: [],
    automod: {
      enabled: false,
      logChannelId: '',
      exemptUsers: [],
      exemptRoles: [],
      rules: {
        blockedWords: [],
        blockedRegex: [],
        allowedDomains: [],
        blockInvites: false,
        blockLinks: false,
        maxMentions: 0,
        caps: {
          enabled: false,
          minLength: 0,
          ratio: 0.7,
        },
        duplicate: {
          enabled: false,
          windowSeconds: 0,
          maxRepeats: 0,
        },
      },
      actions: {
        delete: false,
        timeoutMinutes: 0,
        warnInChannel: false,
        warnTemplate: 'warn',
        warnDeleteAfterSeconds: 0,
      },
    },
    ...overrides,
  };
}

function withTemporaryEnv(overrides, callback) {
  const previous = {};
  const deletions = new Set();

  for (const [key, value] of Object.entries(overrides)) {
    if (Object.prototype.hasOwnProperty.call(process.env, key)) {
      previous[key] = process.env[key];
    } else {
      deletions.add(key);
    }

    if (value === null || value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = String(value);
    }
  }

  const restore = () => {
    for (const key of Object.keys(overrides)) {
      if (deletions.has(key)) {
        delete process.env[key];
      } else {
        process.env[key] = previous[key];
      }
    }
  };

  try {
    const result = callback();
    if (result && typeof result.then === 'function') {
      return result.finally(restore);
    }
    restore();
    return result;
  } catch (error) {
    restore();
    throw error;
  }
}

function ensureNodeExpectation() {
  const arg = process.argv.find((entry) => entry.startsWith('--expect-node='));
  if (!arg) return;

  const expectedMajor = Number(arg.split('=')[1]);
  const currentMajor = Number(process.versions.node.split('.')[0]);
  assert.strictEqual(
    currentMajor,
    expectedMajor,
    `Expected Node ${expectedMajor}, got ${process.versions.node}`,
  );
}

function smokeRequireModules() {
  const modulePaths = [
    'src/core/announcements.js',
    'src/core/command-manifest.js',
    'src/core/command-sync.js',
    'src/core/config.js',
    'src/core/music.js',
    'src/core/state-migration.js',
    'src/core/owners.js',
    'src/core/paths.js',
    'src/core/state.js',
    'src/core/verification.js',
    'src/events/autoclean.js',
    'src/events/automod.js',
    'src/events/full-logs.js',
    'src/cmds/announce.js',
    'src/cmds/automod.js',
    'src/cmds/cache.js',
    'src/cmds/clean.js',
    'src/cmds/dm.js',
    'src/cmds/help-command.js',
    'src/cmds/help.js',
    'src/cmds/logs.js',
    'src/cmds/music.js',
    'src/cmds/settings.js',
    'src/cmds/verify.js',
  ];

  for (const modulePath of modulePaths) {
    require(path.join(projectRoot, modulePath));
  }
}

function testOperatorPaths() {
  const { resolveOperatorPaths } = require(path.join(projectRoot, 'src/core/paths.js'));

  const defaults = resolveOperatorPaths({});
  assert.strictEqual(defaults.configPath, path.join(projectRoot, 'config.json'));
  assert.strictEqual(defaults.dataDir, path.join(projectRoot, 'cache'));
  assert.strictEqual(defaults.logsDir, path.join(projectRoot, 'logs'));
  assert.strictEqual(defaults.syncCommandsOnStartup, true);

  const custom = resolveOperatorPaths({
    OPERATOR_CONFIG_PATH: 'runtime/config/operator.json',
    OPERATOR_DATA_DIR: 'runtime/data',
    OPERATOR_LOG_DIR: 'runtime/logs',
    OPERATOR_SYNC_COMMANDS_ON_STARTUP: '0',
  });
  assert.strictEqual(custom.configPath, path.join(projectRoot, 'runtime', 'config', 'operator.json'));
  assert.strictEqual(custom.serverCacheDir, path.join(projectRoot, 'runtime', 'data', 'server'));
  assert.strictEqual(custom.musicQueueDir, path.join(projectRoot, 'runtime', 'data', 'music-queue'));
  assert.strictEqual(custom.serverLogsDir, path.join(projectRoot, 'runtime', 'logs', 'server'));
  assert.strictEqual(custom.syncCommandsOnStartup, false);
}

function testOwnerResolution() {
  const owners = require(path.join(projectRoot, 'src/core/owners.js'));
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'operator-owner-test-'));
  const configPath = path.join(tmpDir, 'config.json');

  fs.writeFileSync(
    configPath,
    `${JSON.stringify(createValidConfig({
      ownerIds: ['333333333333333333', '444444444444444444'],
    }), null, 2)}\n`,
    'utf8',
  );

  withTemporaryEnv({
    OWNER_IDS: '111111111111111111, 222222222222222222 invalid-owner',
    OWNER_ID: null,
  }, () => {
    const resolved = owners.resolveOwnerIds({ configPath });
    assert.strictEqual(resolved.source, 'env:OWNER_IDS');
    assert.deepStrictEqual([...resolved.ids].sort(), [
      '111111111111111111',
      '222222222222222222',
    ]);
    assert.deepStrictEqual(resolved.invalid, ['invalid-owner']);
    assert.strictEqual(owners.isOwner('111111111111111111', { configPath }), true);
    assert.strictEqual(owners.isOwner('333333333333333333', { configPath }), false);
  });

  withTemporaryEnv({
    OWNER_IDS: null,
    OWNER_ID: null,
  }, () => {
    const resolved = owners.resolveOwnerIds({ configPath });
    assert.strictEqual(resolved.source, 'config:ownerIds');
    assert.deepStrictEqual([...resolved.ids].sort(), [
      '333333333333333333',
      '444444444444444444',
    ]);
    assert.strictEqual(owners.isOwner('333333333333333333', { configPath }), true);
  });

  withTemporaryEnv({
    OWNER_IDS: null,
    OWNER_ID: null,
  }, () => {
    const resolved = owners.resolveOwnerIds({
      configPath: path.join(tmpDir, 'missing-config.json'),
    });
    assert.strictEqual(resolved.source, 'fallback-empty');
    assert.strictEqual(resolved.ids.size, 0);
  });
}

async function testOwnerOnlyCommandGuard() {
  const { executeCleanCommand } = require(path.join(projectRoot, 'src/cmds/clean.js'));

  const cleaner = {
    getStatusSnapshot() {
      return {
        enabled: true,
        schedule: '0 3 * * 0',
        timezone: 'Europe/Warsaw',
        nextRunText: 'soon',
        nextRunAt: new Date().toISOString(),
        dryRun: false,
        deleteIntervalText: '7 dni',
        deleteMessagesOlderThanDays: 0,
        deleteLimit: 100,
        deletePinned: false,
        whitelistUsers: 0,
        whitelistRoles: 0,
        notifyBeforeDelete: 'off',
        logChannelId: '',
        permanentChannels: [],
        queuedChannelsCount: 0,
        queuedChannels: [],
        lastRunAt: null,
        lastTrigger: null,
        lastDeleted: 0,
        lastDurationMs: null,
        lastError: null,
        runs: 0,
        totalDeleted: 0,
      };
    },
  };

  function makeInteraction(userId) {
    const replies = [];

    return {
      user: { id: userId },
      guildId: '123456789012345678',
      channel: {
        id: '999999999999999999',
        isTextBased() {
          return true;
        },
      },
      inGuild() {
        return true;
      },
      options: {
        getString(name) {
          if (name === 'mode') return 'status';
          return null;
        },
      },
      async reply(payload) {
        replies.push(payload);
      },
      get replies() {
        return replies;
      },
    };
  }

  await withTemporaryEnv({
    OWNER_IDS: '999999999999999999',
    OWNER_ID: null,
  }, async () => {
    const denied = makeInteraction('111111111111111111');
    await executeCleanCommand(denied, { cleaner });
    assert.match(String(denied.replies[0]?.content || ''), /tylko dla ownera/i);

    const allowed = makeInteraction('999999999999999999');
    await executeCleanCommand(allowed, { cleaner });
    assert.match(String(allowed.replies[0]?.content || ''), /AutoClean:/i);
  });
}

async function testHelpPanel() {
  const {
    buildHelpPanelPayload,
    executeHelpComponentInteraction,
  } = require(path.join(projectRoot, 'src/cmds/help.js'));
  const { executeHelpCommand } = require(path.join(projectRoot, 'src/cmds/help-command.js'));

  function makeHelpInteraction(userId) {
    const replies = [];
    return {
      user: { id: userId },
      inGuild() {
        return true;
      },
      async reply(payload) {
        replies.push(payload);
      },
      get replies() {
        return replies;
      },
    };
  }

  await withTemporaryEnv({
    OWNER_IDS: '999999999999999999',
    OWNER_ID: null,
  }, async () => {
    const regularPayload = buildHelpPanelPayload('111111111111111111');
    const regularText = extractPanelText(regularPayload);
    assert.match(regularText, /\/announce \.\.\./i);
    assert.match(regularText, /narzedzia ownera/i);
    assert.strictEqual(
      regularPayload.components[0].components[1].components.find((entry) => entry.label === 'Owner').disabled,
      true,
    );

    const ownerMusicText = extractPanelText(buildHelpPanelPayload('999999999999999999', 'music'));
    assert.match(ownerMusicText, /\/music mode:clear\|reconnect/i);

    const ownerModerationText = extractPanelText(buildHelpPanelPayload('999999999999999999', 'moderation'));
    assert.match(ownerModerationText, /\/automod rule list\|add\|remove/i);
    assert.match(ownerModerationText, /\/dm user:<user>\|role:<role> message:<text>/i);
    assert.match(ownerModerationText, /\/logs mode:setup\|status\|enable\|disable/i);

    const ownerOwnerText = extractPanelText(buildHelpPanelPayload('999999999999999999', 'owner'));
    assert.match(ownerOwnerText, /\/clean mode:now\|set\|reset\|status/i);
    assert.match(ownerOwnerText, /\/announce mode:profile_create\|profile_list\|profile_edit\|profile_delete/i);

    const helpInteraction = makeHelpInteraction('999999999999999999');
    await executeHelpCommand(helpInteraction, {
      buildHelpPayload: buildHelpPanelPayload,
    });
    assert.match(extractPanelText(helpInteraction.replies[0]), /Operator Help/i);

    const foreignPanelInteraction = {
      customId: 'help_panel:music:999999999999999999',
      user: { id: '111111111111111111' },
      isButton() {
        return true;
      },
      async reply(payload) {
        foreignPanelInteraction.replyPayload = payload;
      },
    };
    const consumedForeign = await executeHelpComponentInteraction(foreignPanelInteraction);
    assert.strictEqual(consumedForeign, true);
    assert.match(String(foreignPanelInteraction.replyPayload?.content || ''), /innego uzytkownika/i);

    const ownPanelInteraction = {
      customId: 'help_panel:settings:999999999999999999',
      user: { id: '999999999999999999' },
      isButton() {
        return true;
      },
      async update(payload) {
        ownPanelInteraction.updatePayload = payload;
      },
    };
    const consumedOwn = await executeHelpComponentInteraction(ownPanelInteraction);
    assert.strictEqual(consumedOwn, true);
    assert.match(extractPanelText(ownPanelInteraction.updatePayload), /\/debug mode:<music\|permissions\|logs>/i);
  });
}

function extractPanelText(payload) {
  return (payload?.components || [])
    .flatMap((container) => container.components || [])
    .map((component) => component.content || '')
    .join('\n');
}

function extractReplyText(payload) {
  if (typeof payload === 'string') return payload;
  return payload?.content || extractPanelText(payload) || '';
}

function makePermissionHolder(bits = []) {
  return new PermissionsBitField(bits);
}

function makeChannel({ id, name, text = true, voice = false, perms = [] }) {
  return {
    id,
    name,
    isTextBased() {
      return text;
    },
    isVoiceBased() {
      return voice;
    },
    permissionsFor() {
      return makePermissionHolder(perms);
    },
    toString() {
      return `<#${id}>`;
    },
  };
}

function createDiagnosticsInteraction({
  commandName,
  mode = null,
  channel = null,
  guild,
  memberVoiceChannel = null,
}) {
  const replies = [];
  return {
    commandName,
    guild,
    guildId: guild.id,
    channel: channel || guild.channels.cache.get('text-main') || null,
    client: {
      user: {
        tag: 'Operator#0001',
      },
    },
    member: {
      voice: {
        channel: memberVoiceChannel,
      },
    },
    inGuild() {
      return true;
    },
    options: {
      getString(name, required) {
        if (name === 'mode') {
          if (required && !mode) {
            throw new Error('mode missing');
          }
          return mode;
        }
        return null;
      },
      getChannel(name) {
        if (name === 'channel') {
          return channel;
        }
        return null;
      },
    },
    async reply(payload) {
      replies.push(payload);
    },
    get replies() {
      return replies;
    },
  };
}

async function testDiagnosticsCommands() {
  const {
    executeHealthCommand,
    executeDebugCommand,
  } = require(path.join(projectRoot, 'src/cmds/diagnostics.js'));

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'operator-diagnostics-test-'));
  const configPath = path.join(tmpDir, 'config.json');
  fs.writeFileSync(
    configPath,
    `${JSON.stringify(createValidConfig({
      ownerIds: ['555555555555555555'],
    }), null, 2)}\n`,
    'utf8',
  );

  const textMain = makeChannel({
    id: 'text-main',
    name: 'general',
    text: true,
    perms: [
      PermissionFlagsBits.ViewChannel,
      PermissionFlagsBits.SendMessages,
      PermissionFlagsBits.ReadMessageHistory,
    ],
  });
  const logsChannel = makeChannel({
    id: 'logs-main',
    name: 'logs',
    text: true,
    perms: [
      PermissionFlagsBits.ViewChannel,
      PermissionFlagsBits.SendMessages,
      PermissionFlagsBits.ReadMessageHistory,
    ],
  });
  const voiceChannel = makeChannel({
    id: 'voice-main',
    name: 'General',
    text: false,
    voice: true,
    perms: [
      PermissionFlagsBits.ViewChannel,
      PermissionFlagsBits.Connect,
      PermissionFlagsBits.Speak,
    ],
  });

  const guild = {
    id: '123456789012345678',
    members: {
      me: {
        permissions: makePermissionHolder([
          PermissionFlagsBits.ViewAuditLog,
          PermissionFlagsBits.ManageRoles,
          PermissionFlagsBits.ModerateMembers,
        ]),
      },
      fetchMe: async () => guild.members.me,
    },
    channels: {
      cache: new Map([
        ['text-main', textMain],
        ['logs-main', logsChannel],
        ['voice-main', voiceChannel],
      ]),
      fetch: async (id) => guild.channels.cache.get(id) || null,
    },
  };

  const healthyMusicManager = {
    getDiagnostics() {
      return {
        initialized: true,
        usable: true,
        defaultSearchSource: 'ytmsearch',
        recoverySources: ['ytsearch', 'ytmsearch'],
        defaultVolume: 80,
        configuredNodes: [{ id: 'operator-main', host: '127.0.0.1', port: 2333 }],
        config: {
          usesUrl: false,
          hasExplicitHost: true,
          hasExplicitPort: true,
          hasExplicitPassword: true,
          hasExplicitSecure: true,
          hasExplicitNodeId: true,
          hasExplicitSearchSource: true,
        },
        nodes: [{
          id: 'operator-main',
          host: '127.0.0.1',
          port: 2333,
          connected: true,
          playingPlayers: 1,
          players: 1,
          cpuLoad: 0.12,
        }],
        connectedNodes: 1,
        queueStateDirExists: true,
        guildSession: {
          voiceChannelId: 'voice-main',
          playerConnected: true,
          playing: true,
          paused: false,
          currentTrackTitle: 'Test Track',
          queueSize: 2,
          statusText: 'Gra: Test Track',
          cachedStatusText: 'Gra: Test Track',
          hasSavedQueue: false,
        },
      };
    },
  };

  const unhealthyMusicManager = {
    getDiagnostics() {
      return {
        initialized: false,
        usable: false,
        defaultSearchSource: 'ytmsearch',
        recoverySources: [],
        defaultVolume: 80,
        configuredNodes: [{ id: 'operator-main', host: '127.0.0.1', port: 2333 }],
        config: {
          usesUrl: false,
          hasExplicitHost: false,
          hasExplicitPort: false,
          hasExplicitPassword: false,
          hasExplicitSecure: false,
          hasExplicitNodeId: false,
          hasExplicitSearchSource: false,
        },
        nodes: [],
        connectedNodes: 0,
        queueStateDirExists: true,
        guildSession: null,
      };
    },
  };

  const healthyFullLogs = {
    getLogsState() {
      return {
        enabled: true,
        channelId: 'logs-main',
        retentionDays: 30,
        includeBots: false,
        forwardMode: 'all',
      };
    },
    getDiagnostics() {
      return {
        started: true,
        logsDir: path.join(tmpDir, 'logs'),
        logsDirExists: true,
        state: this.getLogsState(),
        canViewAuditLog: true,
        bufferedWrites: 0,
        deliveryQueueSize: 0,
        writeQueueSize: 0,
        auditCacheEntries: 1,
        auditFetchInflight: 0,
        retentionTimerActive: true,
        retentionSweepActive: false,
      };
    },
  };

  const disabledLogs = {
    getLogsState() {
      return {
        enabled: false,
        channelId: null,
        retentionDays: 30,
        includeBots: false,
        forwardMode: 'all',
      };
    },
    getDiagnostics() {
      return {
        started: true,
        logsDir: path.join(tmpDir, 'logs'),
        logsDirExists: true,
        state: this.getLogsState(),
        canViewAuditLog: true,
        bufferedWrites: 0,
        deliveryQueueSize: 0,
        writeQueueSize: 0,
        auditCacheEntries: 0,
        auditFetchInflight: 0,
        retentionTimerActive: true,
        retentionSweepActive: false,
      };
    },
  };

  const healthInteraction = createDiagnosticsInteraction({
    commandName: 'health',
    guild,
    channel: textMain,
    memberVoiceChannel: voiceChannel,
  });
  await withTemporaryEnv({
    OWNER_IDS: '555555555555555555',
    OWNER_ID: null,
    LAVALINK_HOST: '127.0.0.1',
    LAVALINK_PORT: '2333',
    LAVALINK_PASSWORD: 'secret',
    LAVALINK_SECURE: 'false',
    LAVALINK_NODE_ID: 'operator-main',
    LAVALINK_SEARCH_SOURCE: 'ytmsearch',
  }, async () => {
    await executeHealthCommand(healthInteraction, {
      musicManager: healthyMusicManager,
      fullLogs: healthyFullLogs,
      configPath,
    });
  });
  const healthText = extractPanelText(healthInteraction.replies[0]);
  if (Number(process.versions.node.split('.')[0]) === 22) {
    assert.match(healthText, /Stan ogolny: ✅ OK/i);
  } else {
    assert.match(healthText, /Stan ogolny: ⚠️ Uwaga/i);
    assert.match(healthText, /Node\.js: .*target 22\.x/i);
  }

  const musicInteraction = createDiagnosticsInteraction({
    commandName: 'debug',
    mode: 'music',
    guild,
    channel: textMain,
  });
  await withTemporaryEnv({
    OWNER_IDS: null,
    OWNER_ID: null,
    LAVALINK_HOST: null,
    LAVALINK_PORT: null,
    LAVALINK_PASSWORD: null,
    LAVALINK_SECURE: null,
    LAVALINK_NODE_ID: null,
    LAVALINK_SEARCH_SOURCE: null,
  }, async () => {
    await executeDebugCommand(musicInteraction, {
      musicManager: unhealthyMusicManager,
      fullLogs: healthyFullLogs,
      configPath: path.join(tmpDir, 'missing-owner-config.json'),
    });
  });
  const musicText = extractPanelText(musicInteraction.replies[0]);
  assert.match(musicText, /Lavalink jest offline/i);
  assert.match(musicText, /LAVALINK_PASSWORD nie jest ustawiony jawnie/i);

  const badTextChannel = makeChannel({
    id: 'text-bad',
    name: 'restricted',
    text: true,
    perms: [
      PermissionFlagsBits.ViewChannel,
    ],
  });
  guild.channels.cache.set('text-bad', badTextChannel);
  const permissionsInteraction = createDiagnosticsInteraction({
    commandName: 'debug',
    mode: 'permissions',
    guild,
    channel: badTextChannel,
  });
  await executeDebugCommand(permissionsInteraction, {
    musicManager: healthyMusicManager,
    fullLogs: healthyFullLogs,
    configPath,
  });
  assert.match(extractPanelText(permissionsInteraction.replies[0]), /Brakuje: Send Messages, Read Message History/i);

  const logsInteraction = createDiagnosticsInteraction({
    commandName: 'debug',
    mode: 'logs',
    guild,
    channel: textMain,
  });
  await executeDebugCommand(logsInteraction, {
    musicManager: healthyMusicManager,
    fullLogs: disabledLogs,
    configPath,
  });
  assert.match(extractPanelText(logsInteraction.replies[0]), /Logi sa wylaczone na tym serwerze/i);
}

async function testDmCommand() {
  const { executeDmCommand } = require(path.join(projectRoot, 'src/cmds/dm.js'));

  function createUser(id, username, { bot = false, fail = false } = {}) {
    return {
      id,
      username,
      bot,
      sent: [],
      async send(payload) {
        if (fail) {
          throw new Error('DM closed');
        }
        this.sent.push(payload);
      },
    };
  }

  function createRole(id, name) {
    return {
      id,
      name,
      toString() {
        return `<@&${id}>`;
      },
    };
  }

  function makeDmInteraction({
    userId = 'admin-user',
    isAdmin = true,
    targetUser = null,
    targetRole = null,
    message = 'test message',
    guild = null,
  } = {}) {
    const replies = [];
    return {
      user: { id: userId },
      guild,
      guildId: guild?.id || null,
      memberPermissions: {
        has(permission) {
          return isAdmin && permission === PermissionFlagsBits.Administrator;
        },
      },
      inGuild() {
        return Boolean(guild);
      },
      options: {
        getUser(name) {
          if (name === 'user') return targetUser;
          return null;
        },
        getRole(name) {
          if (name === 'role') return targetRole;
          return null;
        },
        getString(name) {
          if (name === 'message') return message;
          return null;
        },
      },
      async reply(payload) {
        this.replied = true;
        replies.push(payload);
      },
      async deferReply() {
        this.deferred = true;
      },
      async editReply(payload) {
        this.replied = true;
        replies.push(payload);
      },
      get replies() {
        return replies;
      },
    };
  }

  const noPermissionInteraction = makeDmInteraction({
    isAdmin: false,
    guild: { id: '1' },
  });
  await executeDmCommand(noPermissionInteraction);
  assert.match(extractReplyText(noPermissionInteraction.replies[0]), /Brak uprawnien Administratora/i);

  const missingTargetInteraction = makeDmInteraction({
    guild: { id: '1' },
  });
  await executeDmCommand(missingTargetInteraction);
  assert.match(extractReplyText(missingTargetInteraction.replies[0]), /dokladnie jedno pole targetu/i);

  const userTarget = createUser('100', 'single-user');
  const directInteraction = makeDmInteraction({
    guild: { id: '1' },
    targetUser: userTarget,
    message: 'hello direct',
  });
  await executeDmCommand(directInteraction);
  assert.strictEqual(userTarget.sent.length, 1);
  assert.strictEqual(userTarget.sent[0].content, 'hello direct');
  assert.match(extractReplyText(directInteraction.replies[0]), /Wyslano DM do <@100>/i);

  const failingUser = createUser('101', 'closed-dm', { fail: true });
  const failingInteraction = makeDmInteraction({
    guild: { id: '1' },
    targetUser: failingUser,
  });
  await executeDmCommand(failingInteraction);
  assert.match(extractReplyText(failingInteraction.replies[0]), /Nie udalo sie wyslac DM do <@101>/i);

  const role = createRole('200', 'Testers');
  const roleMemberOk = {
    user: createUser('201', 'alpha'),
    roles: {
      cache: new Map([[role.id, role]]),
    },
  };
  roleMemberOk.roles.cache.has = (roleId) => roleId === role.id;

  const roleMemberFail = {
    user: createUser('202', 'beta', { fail: true }),
    roles: {
      cache: new Map([[role.id, role]]),
    },
  };
  roleMemberFail.roles.cache.has = (roleId) => roleId === role.id;

  const botMember = {
    user: createUser('203', 'gamma-bot', { bot: true }),
    roles: {
      cache: new Map([[role.id, role]]),
    },
  };
  botMember.roles.cache.has = (roleId) => roleId === role.id;

  const otherMember = {
    user: createUser('204', 'delta'),
    roles: {
      cache: new Map(),
    },
  };
  otherMember.roles.cache.has = () => false;

  const fetchedMembers = new Map([
    ['201', roleMemberOk],
    ['202', roleMemberFail],
    ['203', botMember],
    ['204', otherMember],
  ]);

  const roleGuild = {
    id: 'guild-role',
    members: {
      async fetch() {
        return fetchedMembers;
      },
    },
  };

  const roleInteraction = makeDmInteraction({
    guild: roleGuild,
    targetRole: role,
    message: 'hello role',
  });
  await executeDmCommand(roleInteraction);
  assert.strictEqual(roleMemberOk.user.sent.length, 1);
  assert.strictEqual(roleMemberOk.user.sent[0].content, 'hello role');
  assert.strictEqual(roleMemberFail.user.sent.length, 0);
  const roleText = extractReplyText(roleInteraction.replies[0]);
  assert.match(roleText, /Wysylka DM do roli <@&200> zakonczona/i);
  assert.match(roleText, /Targety: 2/i);
  assert.match(roleText, /Sukces: 1/i);
  assert.match(roleText, /Bledy: 1/i);
  assert.match(roleText, /DM closed/i);
}

async function testMusicSlashControls() {
  const { executeMusicCommand } = require(path.join(projectRoot, 'src/cmds/music.js'));

  const voiceMain = makeChannel({
    id: 'voice-main',
    name: 'General',
    text: false,
    voice: true,
  });
  const voiceOther = makeChannel({
    id: 'voice-other',
    name: 'Other',
    text: false,
    voice: true,
  });

  const guild = {
    id: '864208642086420864',
  };

  let reconnectCalls = 0;
  let reconnectShouldFail = false;
  let state = {
    guildId: guild.id,
    voiceChannelId: voiceMain.id,
    currentTrack: {
      title: 'Track One',
      durationSec: 180,
      url: 'https://example.com/track-1',
    },
    queue: [
      {
        title: 'Track Two',
        durationSec: 210,
        url: 'https://example.com/track-2',
      },
      {
        title: 'Track Three',
        durationSec: 240,
        url: 'https://example.com/track-3',
      },
      {
        title: 'Track Four',
        durationSec: 150,
        url: 'https://example.com/track-4',
      },
    ],
    playing: true,
    paused: false,
    volume: 80,
    positionSec: 35,
    repeatMode: 'off',
    muted: false,
    autoplay: false,
    filters: {
      bassboost: false,
      nightcore: false,
      vaporwave: false,
      eightD: false,
      karaoke: false,
      softLimiter: false,
      eqPreset: 'flat',
    },
  };

  function cloneStateSnapshot() {
    return state ? JSON.parse(JSON.stringify(state)) : null;
  }

  const musicManager = {
    getQueueSnapshot() {
      return cloneStateSnapshot();
    },
    async pause() {
      if (!state || !state.currentTrack) return null;
      state.paused = true;
      state.playing = false;
      return cloneStateSnapshot();
    },
    async resume() {
      if (!state || !state.currentTrack) return null;
      state.paused = false;
      state.playing = true;
      return cloneStateSnapshot();
    },
    async adjustVolume(_guildId, delta) {
      if (!state) return null;
      state.volume = Math.max(0, Math.min(150, state.volume + Number(delta || 0)));
      state.muted = state.volume === 0;
      return cloneStateSnapshot();
    },
    async removeFromQueue(_guildId, position) {
      if (!state) return null;
      const removed = state.queue.splice(position - 1, 1)[0] || null;
      return {
        position,
        removed,
        snapshot: cloneStateSnapshot(),
      };
    },
    async clearQueue() {
      if (!state) return null;
      const removed = state.queue.length;
      state.queue = [];
      return {
        removed,
        snapshot: cloneStateSnapshot(),
      };
    },
    async reconnectSession() {
      if (!state) return null;
      reconnectCalls += 1;
      if (reconnectShouldFail) {
        throw new Error('Lavalink node nadal jest offline po probie reconnect.');
      }
      return {
        snapshot: cloneStateSnapshot(),
        voiceChannelId: state.voiceChannelId,
        nodeConnected: true,
      };
    },
  };

  function makeMusicInteraction({
    mode,
    value = null,
    position = null,
    memberVoiceChannel = voiceMain,
  }) {
    const replies = [];
    return {
      guild,
      guildId: guild.id,
      user: {
        id: '111111111111111111',
      },
      member: {
        voice: {
          channel: memberVoiceChannel,
        },
      },
      replied: false,
      deferred: false,
      inGuild() {
        return true;
      },
      options: {
        getString(name, required) {
          if (name === 'mode') return mode;
          if (required) {
            throw new Error(`missing ${name}`);
          }
          return null;
        },
        getInteger(name) {
          if (name === 'value') return value;
          if (name === 'position') return position;
          return null;
        },
        getAttachment() {
          return null;
        },
      },
      async reply(payload) {
        this.replied = true;
        replies.push(payload);
      },
      async deferReply() {
        this.deferred = true;
      },
      async editReply(payload) {
        this.replied = true;
        replies.push(payload);
      },
      get replies() {
        return replies;
      },
    };
  }

  const pauseInteraction = makeMusicInteraction({ mode: 'pause' });
  await executeMusicCommand(pauseInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(pauseInteraction.replies[0]), /Wstrzymano: Track One/i);
  assert.strictEqual(state.paused, true);
  assert.strictEqual(state.playing, false);

  const resumeInteraction = makeMusicInteraction({ mode: 'resume' });
  await executeMusicCommand(resumeInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(resumeInteraction.replies[0]), /Wznowiono: Track One/i);
  assert.strictEqual(state.paused, false);
  assert.strictEqual(state.playing, true);

  const volumeUpInteraction = makeMusicInteraction({
    mode: 'volume',
    value: 10,
  });
  await executeMusicCommand(volumeUpInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(volumeUpInteraction.replies[0]), /Glosnosc ustawiona na \*\*90%\*\* \(zmiana \+10\)/i);
  assert.strictEqual(state.volume, 90);

  const volumeDownInteraction = makeMusicInteraction({
    mode: 'volume',
    value: -20,
  });
  await executeMusicCommand(volumeDownInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(volumeDownInteraction.replies[0]), /Glosnosc ustawiona na \*\*70%\*\* \(zmiana -20\)/i);
  assert.strictEqual(state.volume, 70);

  const removeInteraction = makeMusicInteraction({
    mode: 'remove',
    position: 2,
  });
  await executeMusicCommand(removeInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(removeInteraction.replies[0]), /Usunieto z kolejki \(pozycja 2\): Track Three/i);
  assert.deepStrictEqual(state.queue.map((track) => track.title), ['Track Two', 'Track Four']);

  const clearInteraction = makeMusicInteraction({
    mode: 'clear',
  });
  await executeMusicCommand(clearInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(clearInteraction.replies[0]), /Wyczyszczono kolejke\. Usunieto \*\*2\*\* oczekujacych utworow\./i);
  assert.deepStrictEqual(state.queue, []);

  state.queue = [{
    title: 'Track Recovered',
    durationSec: 200,
    url: 'https://example.com/recovered',
  }];
  const reconnectInteraction = makeMusicInteraction({
    mode: 'reconnect',
  });
  await executeMusicCommand(reconnectInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(reconnectInteraction.replies[0]), /Reconnect zakonczony\. Kanal: <#voice-main>\. Node: \*\*online\*\*\./i);
  assert.strictEqual(reconnectCalls, 1);

  reconnectShouldFail = true;
  const reconnectFailInteraction = makeMusicInteraction({
    mode: 'reconnect',
  });
  await executeMusicCommand(reconnectFailInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(reconnectFailInteraction.replies[0]), /Reconnect nie powiodl sie: Lavalink node nadal jest offline po probie reconnect\./i);
  reconnectShouldFail = false;

  const wrongVoiceInteraction = makeMusicInteraction({
    mode: 'volume',
    value: 10,
    memberVoiceChannel: voiceOther,
  });
  await executeMusicCommand(wrongVoiceInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(wrongVoiceInteraction.replies[0]), /Musisz byc na tym samym kanale glosowym co bot\./i);

  state = null;

  const noMusicPauseInteraction = makeMusicInteraction({ mode: 'pause' });
  await executeMusicCommand(noMusicPauseInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(noMusicPauseInteraction.replies[0]), /Nic nie gra\./i);

  const noMusicReconnectInteraction = makeMusicInteraction({ mode: 'reconnect' });
  await executeMusicCommand(noMusicReconnectInteraction, { musicManager, logger: null });
  assert.match(extractReplyText(noMusicReconnectInteraction.replies[0]), /Brak aktywnej sesji muzyki\./i);
}

async function testAutomodRuleTools() {
  const AutoMod = require(path.join(projectRoot, 'src/events/automod.js'));
  const { executeAutomodCommand } = require(path.join(projectRoot, 'src/cmds/automod.js'));

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'operator-automod-tools-'));
  const configPath = path.join(tmpDir, 'config.json');
  fs.writeFileSync(
    configPath,
    `${JSON.stringify(createValidConfig({
      automod: {
        enabled: true,
        logChannelId: '',
        exemptUsers: [],
        exemptRoles: [],
        rules: {
          blockedWords: ['scam'],
          blockedRegex: [],
          allowedDomains: [],
          blockInvites: false,
          blockLinks: false,
          maxMentions: 0,
          caps: {
            enabled: false,
            minLength: 0,
            ratio: 0.7,
          },
          duplicate: {
            enabled: false,
            windowSeconds: 0,
            maxRepeats: 0,
          },
        },
        actions: {
          delete: false,
          timeoutMinutes: 0,
          warnInChannel: false,
          warnTemplate: 'warn',
          warnDeleteAfterSeconds: 0,
        },
      },
    }), null, 2)}\n`,
    'utf8',
  );

  const automod = new AutoMod({
    channels: {
      fetch: async () => null,
    },
    rest: {},
  }, {
    configPath,
  });
  automod.start();

  function makeAutomodInteraction({
    subcommandGroup = null,
    subcommand,
    type = null,
    value = null,
    content = null,
  }) {
    const replies = [];
    return {
      guildId: '135791357913579135',
      memberPermissions: {
        has(bit) {
          return bit === PermissionFlagsBits.Administrator;
        },
      },
      inGuild() {
        return true;
      },
      options: {
        getSubcommandGroup(required) {
          if (required && !subcommandGroup) {
            throw new Error('subcommand group missing');
          }
          return subcommandGroup;
        },
        getSubcommand(required) {
          if (required && !subcommand) {
            throw new Error('subcommand missing');
          }
          return subcommand;
        },
        getString(name, required) {
          const values = {
            type,
            value,
            content,
          };
          const selected = Object.prototype.hasOwnProperty.call(values, name) ? values[name] : null;
          if (required && (selected === null || selected === undefined || selected === '')) {
            throw new Error(`missing ${name}`);
          }
          return selected;
        },
      },
      async reply(payload) {
        replies.push(payload);
      },
      get replies() {
        return replies;
      },
    };
  }

  const listInitialInteraction = makeAutomodInteraction({
    subcommandGroup: 'rule',
    subcommand: 'list',
  });
  await executeAutomodCommand(listInitialInteraction, { automod });
  assert.match(extractReplyText(listInitialInteraction.replies[0]), /Blocked words \(1\)/i);
  assert.match(extractReplyText(listInitialInteraction.replies[0]), /scam/i);

  const addWordInteraction = makeAutomodInteraction({
    subcommandGroup: 'rule',
    subcommand: 'add',
    type: 'word',
    value: 'Spam',
  });
  await executeAutomodCommand(addWordInteraction, { automod });
  assert.match(extractReplyText(addWordInteraction.replies[0]), /Dodano regule Blocked words: `spam`/i);

  const addRegexInteraction = makeAutomodInteraction({
    subcommandGroup: 'rule',
    subcommand: 'add',
    type: 'regex',
    value: '^free nitro$',
  });
  await executeAutomodCommand(addRegexInteraction, { automod });
  assert.match(extractReplyText(addRegexInteraction.replies[0]), /Dodano regule Blocked regex: `\^free nitro\$`/i);

  const testWordInteraction = makeAutomodInteraction({
    subcommand: 'test',
    type: 'word',
    value: 'spam',
    content: 'to jest spam',
  });
  await executeAutomodCommand(testWordInteraction, { automod });
  assert.match(extractReplyText(testWordInteraction.replies[0]), /trafia w te regule/i);
  assert.match(extractReplyText(testWordInteraction.replies[0]), /blocked_word/i);

  const restartedAutomod = new AutoMod({
    channels: {
      fetch: async () => null,
    },
    rest: {},
  }, {
    configPath,
  });
  restartedAutomod.start();

  const listAfterRestartInteraction = makeAutomodInteraction({
    subcommandGroup: 'rule',
    subcommand: 'list',
    type: 'regex',
  });
  await executeAutomodCommand(listAfterRestartInteraction, { automod: restartedAutomod });
  assert.match(extractReplyText(listAfterRestartInteraction.replies[0]), /free nitro/i);

  const testLiveInteraction = makeAutomodInteraction({
    subcommand: 'test',
    content: 'FREE NITRO',
  });
  await executeAutomodCommand(testLiveInteraction, { automod: restartedAutomod });
  assert.match(extractReplyText(testLiveInteraction.replies[0]), /Matched: blocked_regex/i);

  const removeRegexInteraction = makeAutomodInteraction({
    subcommandGroup: 'rule',
    subcommand: 'remove',
    type: 'regex',
    value: '^free nitro$',
  });
  await executeAutomodCommand(removeRegexInteraction, { automod: restartedAutomod });
  assert.match(extractReplyText(removeRegexInteraction.replies[0]), /Usunieto regule Blocked regex: `\^free nitro\$`/i);

  const listAfterRemoveInteraction = makeAutomodInteraction({
    subcommandGroup: 'rule',
    subcommand: 'list',
    type: 'regex',
  });
  await executeAutomodCommand(listAfterRemoveInteraction, { automod: restartedAutomod });
  assert.match(extractReplyText(listAfterRemoveInteraction.replies[0]), /Blocked regex \(0\)/i);

  const persistedConfig = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  assert.deepStrictEqual(persistedConfig.automod.rules.blockedWords.sort(), ['scam', 'spam']);
  assert.deepStrictEqual(persistedConfig.automod.rules.blockedRegex, []);
}

async function testVerifyAdminTools() {
  const { executeVerifyCommand } = require(path.join(projectRoot, 'src/cmds/verify.js'));

  const defaultVerificationState = () => ({
    enabled: false,
    roleId: null,
    panel: {
      channelId: null,
      messageId: null,
      customId: null,
    },
    exceptions: [],
    expirations: {},
    challenges: {},
    lockouts: {},
    lastSweepAt: null,
  });

  function cloneVerificationState(state) {
    return {
      ...defaultVerificationState(),
      ...(state || {}),
      panel: {
        ...defaultVerificationState().panel,
        ...((state && state.panel) || {}),
      },
      exceptions: Array.isArray(state?.exceptions) ? [...state.exceptions] : [],
      expirations: state?.expirations ? { ...state.expirations } : {},
      challenges: state?.challenges ? { ...state.challenges } : {},
      lockouts: state?.lockouts ? { ...state.lockouts } : {},
    };
  }

  function mergeVerificationState(current, patch) {
    const next = cloneVerificationState(current);
    for (const [key, value] of Object.entries(patch || {})) {
      if (Array.isArray(value)) {
        next[key] = [...value];
        continue;
      }
      if (value && typeof value === 'object') {
        if (key === 'panel') {
          next.panel = {
            ...next.panel,
            ...value,
          };
          continue;
        }
        if (key === 'expirations' || key === 'challenges' || key === 'lockouts') {
          next[key] = { ...value };
          continue;
        }
      }
      next[key] = value;
    }
    return next;
  }

  const verificationState = new Map();
  const guildId = '246824682468246824';

  const stateStore = {
    getVerificationState(currentGuildId) {
      return cloneVerificationState(verificationState.get(currentGuildId));
    },
    saveVerificationState(currentGuildId, patchOrUpdater) {
      const current = stateStore.getVerificationState(currentGuildId);
      const patch = typeof patchOrUpdater === 'function'
        ? patchOrUpdater(current, { verification: current })
        : patchOrUpdater;
      const next = mergeVerificationState(current, patch || {});
      verificationState.set(currentGuildId, next);
      return next;
    },
  };

  function makeRole(id, position) {
    return {
      id,
      position,
      toString() {
        return `<@&${id}>`;
      },
    };
  }

  function makeMember(id, roleIds = []) {
    const roleCache = new Set(roleIds);
    return {
      id,
      user: { id, tag: `${id}#0001` },
      roles: {
        cache: roleCache,
        async remove(roleId) {
          roleCache.delete(roleId);
        },
      },
    };
  }

  function makeTextChannelWithStore({ id, name, perms = [] }) {
    const channel = makeChannel({ id, name, text: true, perms });
    const messagesStore = new Map();
    channel.messages = {
      async fetch(messageId) {
        return messagesStore.get(messageId) || null;
      },
    };
    channel.__messagesStore = messagesStore;
    return channel;
  }

  const verifiedRole = makeRole('role-verify', 5);
  const otherRole = makeRole('role-other', 3);
  const rolesCache = new Map([
    [verifiedRole.id, verifiedRole],
    [otherRole.id, otherRole],
  ]);

  const adminChannel = makeTextChannelWithStore({
    id: 'verify-text',
    name: 'verify',
    perms: [
      PermissionFlagsBits.ViewChannel,
      PermissionFlagsBits.SendMessages,
      PermissionFlagsBits.ReadMessageHistory,
    ],
  });

  const userA = { id: '100000000000000001', tag: 'userA#0001' };
  const userB = { id: '100000000000000002', tag: 'userB#0001' };
  const userC = { id: '100000000000000003', tag: 'userC#0001' };
  const userD = { id: '100000000000000004', tag: 'userD#0001' };
  const userE = { id: '100000000000000005', tag: 'userE#0001' };
  const staleUserId = '999999999999999999';

  const memberA = makeMember(userA.id, [verifiedRole.id]);
  const memberB = makeMember(userB.id, [verifiedRole.id]);
  const memberC = makeMember(userC.id, []);
  const memberD = makeMember(userD.id, [verifiedRole.id]);
  const memberE = makeMember(userE.id, [verifiedRole.id]);

  const membersCache = new Map([
    [memberA.id, memberA],
    [memberB.id, memberB],
    [memberC.id, memberC],
    [memberD.id, memberD],
    [memberE.id, memberE],
  ]);

  const guild = {
    id: guildId,
    roles: {
      cache: rolesCache,
      async fetch(id) {
        return rolesCache.get(id) || null;
      },
    },
    members: {
      me: {
        permissions: makePermissionHolder([PermissionFlagsBits.ManageRoles]),
        roles: {
          highest: {
            position: 10,
          },
        },
      },
      async fetchMe() {
        return guild.members.me;
      },
      async fetch(id) {
        if (typeof id === 'string') {
          return membersCache.get(id) || null;
        }
        return membersCache;
      },
    },
    channels: {
      cache: new Map([
        [adminChannel.id, adminChannel],
      ]),
      async fetch(id) {
        return guild.channels.cache.get(id) || null;
      },
    },
  };

  let sentMessageCounter = 0;
  function createPanelMessage(channel) {
    sentMessageCounter += 1;
    const message = {
      id: `verify-message-${sentMessageCounter}`,
      async delete() {
        channel.__messagesStore.delete(message.id);
      },
    };
    channel.__messagesStore.set(message.id, message);
    return message;
  }

  function makeVerifyInteraction({
    mode,
    role = null,
    user = null,
    channel = null,
    currentChannel = adminChannel,
  }) {
    const replies = [];
    return {
      guild,
      guildId,
      channel: currentChannel,
      client: {
        rest: {
          async post(route) {
            const matched = String(route).match(/\/channels\/([^/]+)\/messages$/);
            assert.ok(matched, `Unexpected route: ${route}`);
            const targetChannel = guild.channels.cache.get(matched[1]);
            assert.ok(targetChannel, `Missing target channel for route ${route}`);
            return createPanelMessage(targetChannel);
          },
        },
      },
      memberPermissions: {
        has(bit) {
          return bit === PermissionFlagsBits.Administrator;
        },
      },
      inGuild() {
        return true;
      },
      options: {
        getString(name, required) {
          if (name === 'mode') return mode;
          if (required && !mode) throw new Error('mode missing');
          return null;
        },
        getRole(name) {
          if (name === 'rola') return role;
          return null;
        },
        getUser(name) {
          if (name === 'user') return user;
          return null;
        },
        getChannel(name) {
          if (name === 'channel') return channel;
          return null;
        },
      },
      async deferReply() {},
      async reply(payload) {
        replies.push(payload);
      },
      async editReply(payload) {
        replies.push(payload);
      },
      get replies() {
        return replies;
      },
    };
  }

  const setupInteraction = makeVerifyInteraction({
    mode: 'setup',
    role: verifiedRole,
  });
  await executeVerifyCommand(setupInteraction, { stateStore });
  assert.match(extractReplyText(setupInteraction.replies[0]), /Panel verify utworzony/i);
  const setupPanelMessageId = stateStore.getVerificationState(guildId).panel.messageId;
  assert.ok(setupPanelMessageId);
  assert.strictEqual(adminChannel.__messagesStore.has(setupPanelMessageId), true);

  const setupStatusInteraction = makeVerifyInteraction({ mode: 'status' });
  await executeVerifyCommand(setupStatusInteraction, { stateStore });
  assert.match(extractReplyText(setupStatusInteraction.replies[0]), /Panel status: ok/i);

  const resendInteraction = makeVerifyInteraction({ mode: 'resend_panel' });
  await executeVerifyCommand(resendInteraction, { stateStore });
  assert.match(extractReplyText(resendInteraction.replies[0]), /Panel verify wyslany ponownie/i);
  const resentPanelMessageId = stateStore.getVerificationState(guildId).panel.messageId;
  assert.notStrictEqual(resentPanelMessageId, setupPanelMessageId);
  assert.strictEqual(adminChannel.__messagesStore.has(setupPanelMessageId), false);
  assert.strictEqual(adminChannel.__messagesStore.has(resentPanelMessageId), true);

  stateStore.saveVerificationState(guildId, {
    expirations: {
      [userA.id]: addOneCalendarMonthIsoForTest(new Date()),
    },
    challenges: {
      [userA.id]: {
        roleId: verifiedRole.id,
        code: 'ABC123',
        attempts: 0,
        expiresAt: new Date(Date.now() + (15 * 60 * 1000)).toISOString(),
      },
    },
    lockouts: {
      [userA.id]: new Date(Date.now() + (60 * 60 * 1000)).toISOString(),
    },
  });

  const exceptionAddInteraction = makeVerifyInteraction({
    mode: 'exception_add',
    user: userA,
  });
  await executeVerifyCommand(exceptionAddInteraction, { stateStore });
  assert.match(extractReplyText(exceptionAddInteraction.replies[0]), /Dodano <@100000000000000001> do wyjatkow verify/i);
  let currentVerifyState = stateStore.getVerificationState(guildId);
  assert.deepStrictEqual(currentVerifyState.exceptions, [userA.id]);
  assert.strictEqual(currentVerifyState.expirations[userA.id], undefined);
  assert.strictEqual(currentVerifyState.challenges[userA.id], undefined);
  assert.strictEqual(currentVerifyState.lockouts[userA.id], undefined);

  const exceptionAddStatusInteraction = makeVerifyInteraction({ mode: 'status' });
  await executeVerifyCommand(exceptionAddStatusInteraction, { stateStore });
  assert.match(extractReplyText(exceptionAddStatusInteraction.replies[0]), /Wyjatki: 1/i);

  const exceptionRemoveInteraction = makeVerifyInteraction({
    mode: 'exception_remove',
    user: userA,
  });
  await executeVerifyCommand(exceptionRemoveInteraction, { stateStore });
  assert.match(extractReplyText(exceptionRemoveInteraction.replies[0]), /Usunieto <@100000000000000001> z wyjatkow verify/i);
  currentVerifyState = stateStore.getVerificationState(guildId);
  assert.deepStrictEqual(currentVerifyState.exceptions, []);
  assert.ok(currentVerifyState.expirations[userA.id]);

  const exceptionRemoveStatusInteraction = makeVerifyInteraction({ mode: 'status' });
  await executeVerifyCommand(exceptionRemoveStatusInteraction, { stateStore });
  assert.match(extractReplyText(exceptionRemoveStatusInteraction.replies[0]), /Wyjatki: 0/i);
  assert.match(extractReplyText(exceptionRemoveStatusInteraction.replies[0]), /Aktywne expiracje: 1/i);

  stateStore.saveVerificationState(guildId, {
    challenges: {
      [userA.id]: {
        roleId: verifiedRole.id,
        code: 'ZZZ999',
        attempts: 0,
        expiresAt: new Date(Date.now() + (5 * 60 * 1000)).toISOString(),
      },
    },
    lockouts: {
      [userA.id]: new Date(Date.now() + (30 * 60 * 1000)).toISOString(),
    },
  });

  const expireUserInteraction = makeVerifyInteraction({
    mode: 'expire_user',
    user: userA,
  });
  await executeVerifyCommand(expireUserInteraction, { stateStore });
  assert.match(extractReplyText(expireUserInteraction.replies[0]), /Wygaszono verify dla <@100000000000000001>/i);
  currentVerifyState = stateStore.getVerificationState(guildId);
  assert.strictEqual(currentVerifyState.expirations[userA.id], undefined);
  assert.strictEqual(currentVerifyState.challenges[userA.id], undefined);
  assert.strictEqual(currentVerifyState.lockouts[userA.id], undefined);
  assert.strictEqual(memberA.roles.cache.has(verifiedRole.id), false);

  const expireStatusInteraction = makeVerifyInteraction({ mode: 'status' });
  await executeVerifyCommand(expireStatusInteraction, { stateStore });
  assert.match(extractReplyText(expireStatusInteraction.replies[0]), /Aktywne expiracje: 0/i);

  stateStore.saveVerificationState(guildId, {
    panel: {
      channelId: adminChannel.id,
      messageId: 'ghost-panel-message',
      customId: 'verify_button_v1',
    },
    exceptions: [userB.id, staleUserId],
    expirations: {
      [userB.id]: new Date(Date.now() + (24 * 60 * 60 * 1000)).toISOString(),
      [userC.id]: new Date(Date.now() + (24 * 60 * 60 * 1000)).toISOString(),
      [userD.id]: new Date(Date.now() - (24 * 60 * 60 * 1000)).toISOString(),
      [staleUserId]: new Date(Date.now() + (24 * 60 * 60 * 1000)).toISOString(),
    },
    challenges: {
      [userB.id]: {
        roleId: verifiedRole.id,
        code: 'KEEPME',
        attempts: 0,
        expiresAt: new Date(Date.now() + (10 * 60 * 1000)).toISOString(),
      },
      [userC.id]: {
        roleId: verifiedRole.id,
        code: 'DROPIT',
        attempts: 0,
        expiresAt: new Date(Date.now() - (10 * 60 * 1000)).toISOString(),
      },
    },
    lockouts: {
      [userE.id]: new Date(Date.now() + (20 * 60 * 1000)).toISOString(),
      [staleUserId]: new Date(Date.now() + (20 * 60 * 1000)).toISOString(),
    },
  });

  const cleanupInteraction = makeVerifyInteraction({ mode: 'cleanup_state' });
  await executeVerifyCommand(cleanupInteraction, { stateStore });
  const cleanupText = extractReplyText(cleanupInteraction.replies[0]);
  assert.match(cleanupText, /Cleanup verify zakonczony/i);
  assert.match(cleanupText, /Panel cleanup: wyczyszczono martwe referencje/i);

  currentVerifyState = stateStore.getVerificationState(guildId);
  assert.deepStrictEqual(currentVerifyState.exceptions, [userB.id]);
  assert.strictEqual(currentVerifyState.expirations[userB.id], undefined);
  assert.strictEqual(currentVerifyState.expirations[userC.id], undefined);
  assert.strictEqual(currentVerifyState.expirations[staleUserId], undefined);
  assert.ok(currentVerifyState.expirations[userE.id]);
  assert.strictEqual(currentVerifyState.challenges[userB.id], undefined);
  assert.strictEqual(currentVerifyState.challenges[userC.id], undefined);
  assert.strictEqual(currentVerifyState.lockouts[staleUserId], undefined);
  assert.ok(currentVerifyState.lockouts[userE.id]);
  assert.deepStrictEqual(currentVerifyState.panel, {
    channelId: null,
    messageId: null,
    customId: null,
  });
  assert.strictEqual(memberB.roles.cache.has(verifiedRole.id), true);
  assert.strictEqual(memberD.roles.cache.has(verifiedRole.id), false);

  const cleanupStatusInteraction = makeVerifyInteraction({ mode: 'status' });
  await executeVerifyCommand(cleanupStatusInteraction, { stateStore });
  const cleanupStatusText = extractReplyText(cleanupStatusInteraction.replies[0]);
  assert.match(cleanupStatusText, /Panel channel: brak/i);
  assert.match(cleanupStatusText, /Aktywne expiracje: 1/i);
  assert.match(cleanupStatusText, /Aktywne challenge: 0/i);
  assert.match(cleanupStatusText, /Aktywne blokady: 1/i);
}

function addOneCalendarMonthIsoForTest(baseDate) {
  const date = new Date(baseDate);
  const year = date.getUTCFullYear();
  const month = date.getUTCMonth();
  const day = date.getUTCDate();
  const hours = date.getUTCHours();
  const minutes = date.getUTCMinutes();
  const seconds = date.getUTCSeconds();
  const ms = date.getUTCMilliseconds();

  let targetYear = year;
  let targetMonth = month + 1;
  if (targetMonth > 11) {
    targetMonth = 0;
    targetYear += 1;
  }

  const lastDayOfTargetMonth = new Date(Date.UTC(targetYear, targetMonth + 1, 0)).getUTCDate();
  const targetDay = Math.min(day, lastDayOfTargetMonth);

  return new Date(Date.UTC(targetYear, targetMonth, targetDay, hours, minutes, seconds, ms)).toISOString();
}

async function testLogsAdminTools() {
  const FullLogs = require(path.join(projectRoot, 'src/events/full-logs.js'));
  const { executeLogsCommand } = require(path.join(projectRoot, 'src/cmds/logs.js'));

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'operator-logs-tools-'));
  const logsDir = path.join(tmpDir, 'logs');
  const guildId = '987654321098765432';
  const guildLogDir = path.join(logsDir, guildId);
  fs.mkdirSync(guildLogDir, { recursive: true });

  const state = new Map();
  function getLogsState(currentGuildId) {
    const existing = state.get(currentGuildId) || {};
    return {
      enabled: false,
      channelId: null,
      retentionDays: 30,
      forwardMode: 'all',
      includeBots: false,
      disabledCategories: [],
      ...existing,
    };
  }
  function saveLogsState(currentGuildId, patchOrUpdater) {
    const current = getLogsState(currentGuildId);
    const patch = typeof patchOrUpdater === 'function'
      ? patchOrUpdater(current, { logs: current })
      : patchOrUpdater;
    const next = {
      ...current,
      ...(patch || {}),
    };
    state.set(currentGuildId, next);
    return next;
  }

  saveLogsState(guildId, {
    enabled: true,
    channelId: 'logs-main',
    retentionDays: 30,
    includeBots: false,
    disabledCategories: [],
  });

  const fullLogs = new FullLogs({
    user: { id: 'bot-1' },
    rest: {},
    on() {},
    off() {},
  }, {
    logsDir,
    stateStore: {
      getLogsState,
      saveLogsState,
    },
  });

  const logsChannel = makeChannel({
    id: 'logs-main',
    name: 'logs',
    text: true,
    perms: [
      PermissionFlagsBits.ViewChannel,
      PermissionFlagsBits.SendMessages,
      PermissionFlagsBits.ReadMessageHistory,
    ],
  });
  const textMain = makeChannel({
    id: 'text-main',
    name: 'general',
    text: true,
    perms: [
      PermissionFlagsBits.ViewChannel,
      PermissionFlagsBits.SendMessages,
      PermissionFlagsBits.ReadMessageHistory,
    ],
  });

  const guild = {
    id: guildId,
    channels: {
      cache: new Map([
        ['logs-main', logsChannel],
        ['text-main', textMain],
      ]),
      fetch: async (id) => guild.channels.cache.get(id) || null,
    },
  };

  const todayDay = new Date().toISOString().slice(0, 10);
  const yesterday = new Date();
  yesterday.setUTCDate(yesterday.getUTCDate() - 1);
  const exactDay = yesterday.toISOString().slice(0, 10);
  const oldDay = '2000-01-01';

  const exampleEntry = {
    ts: `${exactDay}T18:53:00.000Z`,
    guildId,
    category: 'message',
    action: 'delete',
    actor: { id: '111111111111111111', tag: 'admin#0001', bot: false },
    target: { kind: 'message', id: '555', ownerId: '222222222222222222', ownerTag: 'user#0002' },
    location: { channelId: 'text-main', channelName: 'general' },
    summary: 'Message deleted.',
    details: ['content=hello'],
    metadata: {
      content: 'hello',
      ownerId: '222222222222222222',
      ownerTag: 'user#0002',
      attachments: [],
    },
  };

  fs.writeFileSync(path.join(guildLogDir, `${exactDay}.jsonl`), `${JSON.stringify(exampleEntry)}\n`, 'utf8');
  fs.writeFileSync(path.join(guildLogDir, `${oldDay}.jsonl`), `${JSON.stringify({ ...exampleEntry, ts: `${oldDay}T01:00:00.000Z` })}\n`, 'utf8');
  fs.writeFileSync(path.join(guildLogDir, `${todayDay}.jsonl`), `${JSON.stringify({ ...exampleEntry, ts: `${todayDay}T10:00:00.000Z` })}\n`, 'utf8');

  function makeLogsInteraction({
    mode,
    channel = null,
    retentionDays = null,
    value = null,
    day = null,
    category = null,
    olderThanDays = null,
    confirm = null,
  }) {
    const replies = [];
    return {
      guildId,
      guild,
      channel: channel || textMain,
      memberPermissions: {
        has(bit) {
          return bit === PermissionFlagsBits.Administrator;
        },
      },
      inGuild() {
        return true;
      },
      options: {
        getString(name, required) {
          const values = {
            mode,
            day,
            category,
          };
          const selected = Object.prototype.hasOwnProperty.call(values, name) ? values[name] : null;
          if (required && (selected === null || selected === undefined || selected === '')) {
            throw new Error(`missing ${name}`);
          }
          return selected;
        },
        getChannel(name) {
          if (name === 'channel') return channel;
          return null;
        },
        getInteger(name) {
          if (name === 'retention_days') return retentionDays;
          if (name === 'older_than_days') return olderThanDays;
          return null;
        },
        getBoolean(name) {
          if (name === 'value') return value;
          if (name === 'confirm') return confirm;
          return null;
        },
      },
      async reply(payload) {
        replies.push(payload);
      },
      get replies() {
        return replies;
      },
      user: { id: '333333333333333333', tag: 'tester#0001', bot: false },
    };
  }

  const previewInteraction = makeLogsInteraction({ mode: 'preview' });
  await executeLogsCommand(previewInteraction, {
    stateStore: { getLogsState, saveLogsState },
    fullLogs,
  });
  const previewText = extractPanelText(previewInteraction.replies[0]);
  assert.match(previewText, /Logs Preview/i);
  assert.match(previewText, /Podglad bazuje na ostatnim zapisanym wpisie/i);

  const exportExistingInteraction = makeLogsInteraction({
    mode: 'export_day',
    day: exactDay,
  });
  await executeLogsCommand(exportExistingInteraction, {
    stateStore: { getLogsState, saveLogsState },
    fullLogs,
  });
  assert.strictEqual(Array.isArray(exportExistingInteraction.replies[0].files), true);
  assert.strictEqual(exportExistingInteraction.replies[0].files.length, 1);

  const exportMissingInteraction = makeLogsInteraction({
    mode: 'export_day',
    day: '1999-01-01',
  });
  await executeLogsCommand(exportMissingInteraction, {
    stateStore: { getLogsState, saveLogsState },
    fullLogs,
  });
  assert.match(extractPanelText(exportMissingInteraction.replies[0]), /Brak logu dla dnia/i);

  const disableCategoryInteraction = makeLogsInteraction({
    mode: 'categories',
    category: 'message',
    value: false,
  });
  await executeLogsCommand(disableCategoryInteraction, {
    stateStore: { getLogsState, saveLogsState },
    fullLogs,
  });
  assert.deepStrictEqual(getLogsState(guildId).disabledCategories, ['message']);

  const enableCategoryInteraction = makeLogsInteraction({
    mode: 'categories',
    category: 'message',
    value: true,
  });
  await executeLogsCommand(enableCategoryInteraction, {
    stateStore: { getLogsState, saveLogsState },
    fullLogs,
  });
  assert.deepStrictEqual(getLogsState(guildId).disabledCategories, []);

  const purgeDayInteraction = makeLogsInteraction({
    mode: 'purge',
    day: exactDay,
    confirm: true,
  });
  await executeLogsCommand(purgeDayInteraction, {
    stateStore: { getLogsState, saveLogsState },
    fullLogs,
  });
  assert.strictEqual(fs.existsSync(path.join(guildLogDir, `${exactDay}.jsonl`)), false);
  assert.strictEqual(fs.existsSync(path.join(guildLogDir, `${todayDay}.jsonl`)), true);

  const purgeOlderInteraction = makeLogsInteraction({
    mode: 'purge',
    olderThanDays: 30,
    confirm: true,
  });
  await executeLogsCommand(purgeOlderInteraction, {
    stateStore: { getLogsState, saveLogsState },
    fullLogs,
  });
  assert.strictEqual(fs.existsSync(path.join(guildLogDir, `${oldDay}.jsonl`)), false);
  assert.strictEqual(fs.existsSync(path.join(guildLogDir, `${todayDay}.jsonl`)), true);
}

async function testStateMigrationLab() {
  const {
    buildStateSnapshot,
    compareStateSnapshots,
    createStateBackup,
    readStateSnapshotFromSqlite,
    restoreStateBackup,
    writeStateSnapshotToSqlite,
  } = require(path.join(projectRoot, 'src/core/state-migration.js'));
  const { initServerStateStore } = require(path.join(projectRoot, 'src/core/state.js'));

  let sqliteAvailable = true;
  try {
    require('node:sqlite');
  } catch {
    sqliteAvailable = false;
  }
  if (!sqliteAvailable) {
    return;
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'operator-state-migration-test-'));
  const serverCacheDir = path.join(tmpDir, 'cache', 'server');
  const musicQueueDir = path.join(tmpDir, 'cache', 'music-queue');
  const userCacheDir = path.join(tmpDir, 'cache', 'user');
  const backupDir = path.join(tmpDir, 'backup');
  const dbPath = path.join(tmpDir, 'state-lab.sqlite');

  fs.mkdirSync(serverCacheDir, { recursive: true });
  fs.mkdirSync(musicQueueDir, { recursive: true });
  fs.mkdirSync(path.join(userCacheDir, 'user-1'), { recursive: true });

  const stateStore = initServerStateStore({ cacheDir: serverCacheDir, logger: null });
  const guildId = '111111111111111111';
  stateStore.saveGuildState(guildId, {
    guildId,
    guildName: 'Migration Test Guild',
    autoclean: {
      nextRunAt: '2026-06-03T01:00:00.000Z',
      queuedChannels: [
        {
          channelId: '222222222222222222',
          queuedAt: '2026-06-02T10:00:00.000Z',
          queuedBy: '333333333333333333',
        },
      ],
    },
    verification: {
      enabled: true,
      roleId: '444444444444444444',
      exceptions: ['555555555555555555'],
      expirations: {
        '666666666666666666': '2026-07-01T00:00:00.000Z',
      },
      challenges: {
        '777777777777777777': {
          roleId: '444444444444444444',
          code: 'ABCD12',
          attempts: 1,
          expiresAt: '2026-06-02T12:00:00.000Z',
        },
      },
    },
    announcements: {
      webhooks: {
        '888888888888888888': {
          id: '999999999999999999',
          token: 'token-value',
        },
      },
      profiles: {
        release: {
          channelId: '888888888888888888',
          title: 'Release',
          color: '#2F9E44',
          username: 'Operator',
          avatarURL: null,
          createdBy: '333333333333333333',
          createdAt: '2026-06-02T09:00:00.000Z',
          updatedAt: '2026-06-02T09:00:00.000Z',
        },
      },
      items: [
        {
          id: 'ann_test_1',
          code: 'TEST',
          channelId: '888888888888888888',
          deliveryChannelId: '888888888888888888',
          runAt: '2026-06-02T10:00:00.000Z',
          content: 'hello',
          sendMode: 'message',
          profileName: 'release',
          title: null,
          color: null,
          username: 'Operator',
          avatarURL: null,
          buttonLabel: null,
          buttonURL: null,
          status: 'sent',
          createdBy: '333333333333333333',
          createdAt: '2026-06-02T09:00:00.000Z',
          updatedAt: '2026-06-02T10:00:00.000Z',
          sentAt: '2026-06-02T10:00:00.000Z',
          messageId: '100000000000000001',
          messageIds: ['100000000000000001'],
          webhookId: '999999999999999999',
          lastError: null,
        },
      ],
    },
    logs: {
      enabled: true,
      channelId: '101010101010101010',
      retentionDays: 14,
      forwardMode: 'all',
      includeBots: false,
      disabledCategories: ['voice'],
    },
  });

  fs.writeFileSync(path.join(musicQueueDir, `${guildId}.json`), `${JSON.stringify({
    guildId,
    voiceChannelId: 'voice-main',
    savedAt: Date.now(),
    volume: 80,
    repeatMode: 'off',
    autoplay: true,
    tracks: [
      { url: 'https://example.test/1', title: 'Track One' },
      { url: 'https://example.test/2', title: 'Track Two' },
    ],
  }, null, 2)}\n`, 'utf8');

  fs.writeFileSync(path.join(userCacheDir, 'user-1', 'commands.json'), `${JSON.stringify({
    total: 3,
    lastCommand: 'music play',
    lastUsedAt: '2026-06-02T11:11:11.111Z',
  }, null, 2)}\n`, 'utf8');

  const initialSnapshot = buildStateSnapshot({
    serverCacheDir,
    musicQueueDir,
    userCacheDir,
    generatedAt: '2026-06-02T12:00:00.000Z',
  });

  const manifest = createStateBackup({
    serverCacheDir,
    musicQueueDir,
    userCacheDir,
    backupDir,
    createdAt: '2026-06-02T12:01:00.000Z',
  });
  assert.strictEqual(Array.isArray(manifest.files), true);
  assert.strictEqual(manifest.files.length >= 3, true);
  assert.strictEqual(fs.existsSync(path.join(backupDir, 'manifest.json')), true);

  writeStateSnapshotToSqlite({
    dbPath,
    snapshot: initialSnapshot,
    meta: {
      smokeTest: true,
    },
  });

  const sqliteSnapshot = readStateSnapshotFromSqlite({ dbPath });
  const comparison = compareStateSnapshots(initialSnapshot, sqliteSnapshot);
  assert.strictEqual(comparison.equal, true, comparison.differences.join('\n'));

  const sqliteSnapshotAfterRestart = readStateSnapshotFromSqlite({ dbPath });
  const restartComparison = compareStateSnapshots(initialSnapshot, sqliteSnapshotAfterRestart);
  assert.strictEqual(restartComparison.equal, true, restartComparison.differences.join('\n'));

  stateStore.saveGuildState(guildId, {
    guildName: 'Corrupted After Backup',
  });
  fs.rmSync(path.join(userCacheDir, 'user-1', 'commands.json'), { force: true });
  fs.writeFileSync(path.join(musicQueueDir, `${guildId}.json`), `${JSON.stringify({
    guildId,
    tracks: [],
  }, null, 2)}\n`, 'utf8');

  restoreStateBackup({
    backupDir,
    serverCacheDir,
    musicQueueDir,
    userCacheDir,
  });

  const restoredSnapshot = buildStateSnapshot({
    serverCacheDir,
    musicQueueDir,
    userCacheDir,
    generatedAt: '2026-06-02T12:05:00.000Z',
  });
  const rollbackComparison = compareStateSnapshots(
    { ...initialSnapshot, generatedAt: restoredSnapshot.generatedAt },
    restoredSnapshot,
  );
  assert.strictEqual(rollbackComparison.equal, true, rollbackComparison.differences.join('\n'));
}

function formatAnnouncementUtcInput(date) {
  const value = new Date(date);
  const year = value.getUTCFullYear();
  const month = String(value.getUTCMonth() + 1).padStart(2, '0');
  const day = String(value.getUTCDate()).padStart(2, '0');
  const hours = String(value.getUTCHours()).padStart(2, '0');
  const minutes = String(value.getUTCMinutes()).padStart(2, '0');
  return `${year}-${month}-${day} ${hours}:${minutes}`;
}

async function testAnnouncementTools() {
  const { executeAnnounceCommand } = require(path.join(projectRoot, 'src/cmds/announce.js'));
  const { runAnnouncementSweep } = require(path.join(projectRoot, 'src/core/announcements.js'));
  const { initServerStateStore } = require(path.join(projectRoot, 'src/core/state.js'));

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'operator-announce-tools-'));
  const cacheDir = path.join(tmpDir, 'cache');
  const guildId = '246824682468246824';
  const ownerId = '555555555555555555';
  const webhookRegistry = new Map();
  let webhookCounter = 0;
  let messageCounter = 0;

  function makeAnnouncementChannel(id, name) {
    const channel = makeChannel({
      id,
      name,
      text: true,
      perms: [
        PermissionFlagsBits.ViewChannel,
        PermissionFlagsBits.SendMessages,
        PermissionFlagsBits.ReadMessageHistory,
      ],
    });

    channel.__messagesStore = new Map();
    channel.__sendLog = [];
    channel.createWebhook = async () => {
      webhookCounter += 1;
      const webhookId = `webhook-${channel.id}-${webhookCounter}`;
      const webhook = {
        id: webhookId,
        token: `token-${webhookId}`,
        channelId: channel.id,
        async send(payload) {
          const content = payload.content || payload.embeds?.[0]?.data?.description || '';
          if (content === 'Sweep me once') {
            await new Promise((resolve) => setTimeout(resolve, 25));
          }

          messageCounter += 1;
          const message = {
            id: `announcement-message-${messageCounter}`,
            channelId: channel.id,
            payload: { ...payload },
          };
          channel.__messagesStore.set(message.id, message);
          channel.__sendLog.push({
            messageId: message.id,
            content,
          });
          return message;
        },
        async editMessage(messageId, payload) {
          const existing = channel.__messagesStore.get(String(messageId));
          assert.ok(existing, `Missing announcement message ${messageId}`);
          existing.payload = { ...payload };
          return existing;
        },
        async deleteMessage(messageId) {
          const deleted = channel.__messagesStore.delete(String(messageId));
          if (!deleted) {
            throw new Error(`Missing announcement message ${messageId}`);
          }
        },
      };

      webhookRegistry.set(webhook.id, webhook);
      return webhook;
    };

    return channel;
  }

  const announceMain = makeAnnouncementChannel('announce-main', 'announcements');
  const announceAlt = makeAnnouncementChannel('announce-alt', 'announcements-2');
  const guild = {
    id: guildId,
    channels: {
      cache: new Map([
        [announceMain.id, announceMain],
        [announceAlt.id, announceAlt],
      ]),
      async fetch(id) {
        return guild.channels.cache.get(id) || null;
      },
    },
  };

  const client = {
    guilds: {
      cache: new Map([
        [guildId, guild],
      ]),
    },
    async fetchWebhook(id, token) {
      const webhook = webhookRegistry.get(String(id));
      if (!webhook) {
        throw new Error(`Unknown webhook ${id}`);
      }
      if (token && token !== webhook.token) {
        throw new Error(`Invalid webhook token for ${id}`);
      }
      return webhook;
    },
  };

  function makeAnnounceInteraction({
    mode,
    datetime = null,
    content = null,
    channel = null,
    id = null,
    title = null,
    color = null,
    username = null,
    avatarURL = null,
    sendMode = null,
    buttonLabel = null,
    buttonURL = null,
    profile = null,
    name = null,
  }) {
    const replies = [];
    return {
      guildId,
      guild,
      client,
      channel: announceMain,
      user: { id: ownerId, tag: 'owner#0001' },
      inGuild() {
        return true;
      },
      options: {
        getString(optionName, required) {
          const values = {
            mode,
            datetime,
            content,
            id,
            title,
            color,
            username,
            avatar_url: avatarURL,
            send_mode: sendMode,
            button_label: buttonLabel,
            button_url: buttonURL,
            profile,
            name,
          };
          const selected = Object.prototype.hasOwnProperty.call(values, optionName) ? values[optionName] : null;
          if (required && (selected === null || selected === undefined || selected === '')) {
            throw new Error(`missing ${optionName}`);
          }
          return selected;
        },
        getChannel(optionName) {
          if (optionName === 'channel') {
            return channel;
          }
          return null;
        },
      },
      async reply(payload) {
        replies.push(typeof payload === 'string' ? { content: payload } : payload);
      },
      async deferReply(payload) {
        replies.push({ deferred: true, ...(payload || {}) });
      },
      async editReply(payload) {
        replies.push(typeof payload === 'string' ? { content: payload } : payload);
      },
      get replies() {
        return replies;
      },
    };
  }

  const logger = {
    info() {},
    warn() {},
    error() {},
  };

  await withTemporaryEnv({
    OWNER_IDS: ownerId,
    OWNER_ID: null,
  }, async () => {
    let stateStore = initServerStateStore({ cacheDir, logger: null });

    const futureRunAt = formatAnnouncementUtcInput(Date.now() + 60 * 60 * 1000);
    const createInteraction = makeAnnounceInteraction({
      mode: 'create',
      datetime: futureRunAt,
      content: 'Scheduled hello',
      channel: announceMain,
    });
    await executeAnnounceCommand(createInteraction, { stateStore, logger });
    assert.match(extractReplyText(createInteraction.replies[0]), /Dodano ogloszenie/i);
    assert.strictEqual(stateStore.getAnnouncementsState(guildId).items.length, 1);

    stateStore = initServerStateStore({ cacheDir, logger: null });
    const afterRestart = stateStore.getAnnouncementsState(guildId);
    assert.strictEqual(afterRestart.items.length, 1);
    assert.strictEqual(afterRestart.items[0].status, 'scheduled');

    const nowInteraction = makeAnnounceInteraction({
      mode: 'now',
      content: 'Live hello',
      channel: announceMain,
    });
    await executeAnnounceCommand(nowInteraction, { stateStore, logger });
    assert.match(extractReplyText(nowInteraction.replies[nowInteraction.replies.length - 1]), /Wyslano ogloszenie/i);
    assert.strictEqual(announceMain.__messagesStore.size, 1);

    let currentState = stateStore.getAnnouncementsState(guildId);
    let liveItem = currentState.items.find((item) => item.status === 'sent' && item.content === 'Live hello');
    assert.ok(liveItem);
    assert.strictEqual(liveItem.deliveryChannelId, announceMain.id);
    assert.strictEqual(liveItem.webhookId.startsWith(`webhook-${announceMain.id}`), true);
    assert.strictEqual(Object.prototype.hasOwnProperty.call(liveItem, 'webhookToken'), false);
    const persistedJson = fs.readFileSync(path.join(cacheDir, `${guildId}.json`), 'utf8');
    assert.strictEqual(persistedJson.includes('"webhookToken"'), false);

    const firstLiveMessageId = liveItem.messageId;
    const editInPlaceInteraction = makeAnnounceInteraction({
      mode: 'edit',
      id: liveItem.code,
      content: 'Live hello updated',
    });
    await executeAnnounceCommand(editInPlaceInteraction, { stateStore, logger });
    assert.match(extractReplyText(editInPlaceInteraction.replies[0]), /edycja w miejscu/i);

    currentState = stateStore.getAnnouncementsState(guildId);
    liveItem = currentState.items.find((item) => item.id === liveItem.id);
    assert.ok(liveItem);
    assert.strictEqual(liveItem.messageId, firstLiveMessageId);
    assert.strictEqual(announceMain.__messagesStore.get(firstLiveMessageId).payload.content, 'Live hello updated');

    const moveInteraction = makeAnnounceInteraction({
      mode: 'edit',
      id: liveItem.code,
      content: 'Moved live hello',
      channel: announceAlt,
    });
    await executeAnnounceCommand(moveInteraction, { stateStore, logger });
    assert.match(extractReplyText(moveInteraction.replies[0]), /ponowna publikacja na nowy kanal/i);

    currentState = stateStore.getAnnouncementsState(guildId);
    liveItem = currentState.items.find((item) => item.id === liveItem.id);
    assert.ok(liveItem);
    assert.notStrictEqual(liveItem.messageId, firstLiveMessageId);
    assert.strictEqual(liveItem.deliveryChannelId, announceAlt.id);
    assert.strictEqual(announceMain.__messagesStore.has(firstLiveMessageId), false);
    assert.strictEqual(announceAlt.__messagesStore.has(liveItem.messageId), true);

    const deleteInteraction = makeAnnounceInteraction({
      mode: 'delete',
      id: liveItem.code,
    });
    await executeAnnounceCommand(deleteInteraction, { stateStore, logger });
    assert.match(extractReplyText(deleteInteraction.replies[0]), /Usunieto ogloszenie/i);
    assert.strictEqual(announceAlt.__messagesStore.has(liveItem.messageId), false);
    assert.strictEqual(stateStore.getAnnouncementsState(guildId).items.some((item) => item.id === liveItem.id), false);

    const pastRunAt = formatAnnouncementUtcInput(Date.now() - 60 * 1000);
    const dueCreateInteraction = makeAnnounceInteraction({
      mode: 'create',
      datetime: pastRunAt,
      content: 'Sweep me once',
      channel: announceMain,
    });
    await executeAnnounceCommand(dueCreateInteraction, { stateStore, logger });
    assert.match(extractReplyText(dueCreateInteraction.replies[0]), /Dodano ogloszenie/i);

    const [firstSweep, secondSweep] = await Promise.all([
      runAnnouncementSweep({ client, stateStore, logger }),
      runAnnouncementSweep({ client, stateStore, logger }),
    ]);

    assert.strictEqual(announceMain.__sendLog.filter((entry) => entry.content === 'Sweep me once').length, 1);
    assert.strictEqual(firstSweep.sent + secondSweep.sent >= 1, true);
    assert.strictEqual(firstSweep.joined || secondSweep.joined, true);

    currentState = stateStore.getAnnouncementsState(guildId);
    const sweptItem = currentState.items.find((item) => item.content === 'Sweep me once');
    assert.ok(sweptItem);
    assert.strictEqual(sweptItem.status, 'sent');
  });
}

async function main() {
  ensureNodeExpectation();
  smokeRequireModules();
  testOperatorPaths();
  testOwnerResolution();
  await testOwnerOnlyCommandGuard();
  await testHelpPanel();
  await testDiagnosticsCommands();
  await testDmCommand();
  await testMusicSlashControls();
  await testAutomodRuleTools();
  await testVerifyAdminTools();
  await testLogsAdminTools();
  await testAnnouncementTools();
  await testStateMigrationLab();
  console.log(`Smoke OK on Node ${process.versions.node}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
