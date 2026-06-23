const {
  ApplicationCommandOptionType,
  ChannelType,
  PermissionFlagsBits,
  Routes,
} = require('discord.js');

const VERIFY_BUTTON_CUSTOM_ID = 'verify_button_v1';
const VERIFY_PANEL_FLAGS = 32768;
const VERIFY_PANEL_TITLE = 'Weryfikacja';
const VERIFY_PANEL_BODY = 'Kliknij przycisk ponizej, aby otrzymac role Verified na jeden miesiac kalendarzowy.';

const VERIFY_PANEL_STATE_EMPTY = {
  channelId: null,
  messageId: null,
  customId: null,
};

const verifyCommand = {
  name: 'verify',
  description: 'Panel i status weryfikacji',
  dm_permission: false,
  default_member_permissions: PermissionFlagsBits.Administrator.toString(),
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Tryb komendy verify',
      required: true,
      choices: [
        { name: 'setup', value: 'setup' },
        { name: 'status', value: 'status' },
        { name: 'resend_panel', value: 'resend_panel' },
        { name: 'exception_add', value: 'exception_add' },
        { name: 'exception_remove', value: 'exception_remove' },
        { name: 'expire_user', value: 'expire_user' },
        { name: 'cleanup_state', value: 'cleanup_state' },
      ],
    },
    {
      type: ApplicationCommandOptionType.Role,
      name: 'rola',
      description: 'Rola zweryfikowanego (dla mode=setup)',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.User,
      name: 'user',
      description: 'Uzytkownik dla exception_add/exception_remove/expire_user',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Channel,
      name: 'channel',
      description: 'Kanal panelu dla setup/resend_panel',
      required: false,
      channel_types: [ChannelType.GuildText, ChannelType.GuildAnnouncement],
    },
  ],
};

function addOneCalendarMonthIso(baseDate) {
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

function buildVerifyPanelPayload() {
  return {
    flags: VERIFY_PANEL_FLAGS,
    components: [
      {
        type: 17,
        components: [
          {
            type: 10,
            content: `## ${VERIFY_PANEL_TITLE}`,
          },
          {
            type: 10,
            content: VERIFY_PANEL_BODY,
          },
          {
            type: 1,
            components: [
              {
                type: 2,
                style: 1,
                custom_id: VERIFY_BUTTON_CUSTOM_ID,
                label: 'Zweryfikuj',
              },
            ],
          },
        ],
      },
    ],
  };
}

function replyEphemeral(interaction, content) {
  return interaction.reply({
    content,
    ephemeral: true,
  });
}

async function deferEphemeral(interaction) {
  if (typeof interaction.deferReply === 'function') {
    await interaction.deferReply({ ephemeral: true });
    return true;
  }
  return false;
}

async function finishEphemeral(interaction, deferred, content) {
  if (deferred && typeof interaction.editReply === 'function') {
    await interaction.editReply(content);
    return;
  }
  await replyEphemeral(interaction, content);
}

function dedupeIds(values) {
  if (!Array.isArray(values)) return [];
  return [...new Set(values.map((value) => String(value || '').trim()).filter(Boolean))];
}

function formatUserList(userIds, { empty = 'brak', max = 5 } = {}) {
  const normalized = dedupeIds(userIds);
  if (normalized.length === 0) return empty;
  const preview = normalized.slice(0, max).map((id) => `<@${id}>`).join(', ');
  if (normalized.length <= max) return preview;
  return `${preview} i jeszcze ${normalized.length - max}`;
}

function createPanelState(channelId, messageId, customId = VERIFY_BUTTON_CUSTOM_ID) {
  return {
    channelId: channelId ? String(channelId) : null,
    messageId: messageId ? String(messageId) : null,
    customId: customId ? String(customId) : null,
  };
}

async function fetchBotMember(guild) {
  if (guild.members?.me) {
    return guild.members.me;
  }
  if (typeof guild.members?.fetchMe !== 'function') {
    return null;
  }
  return guild.members.fetchMe().catch(() => null);
}

async function fetchRole(guild, roleId) {
  if (!roleId) return null;
  if (guild.roles?.cache?.get(roleId)) {
    return guild.roles.cache.get(roleId);
  }
  if (typeof guild.roles?.fetch !== 'function') {
    return null;
  }
  return guild.roles.fetch(roleId).catch(() => null);
}

async function fetchMember(guild, userId) {
  if (!userId) return null;
  if (typeof guild.members?.fetch !== 'function') {
    return null;
  }
  return guild.members.fetch(userId).catch(() => null);
}

async function fetchAllMembers(guild) {
  if (typeof guild.members?.fetch !== 'function') {
    return null;
  }
  return guild.members.fetch().catch(() => null);
}

async function inspectPanelState(guild, panelInput) {
  const panel = panelInput || VERIFY_PANEL_STATE_EMPTY;
  if (!panel.channelId || !panel.messageId) {
    return {
      state: 'brak',
      channel: null,
      message: null,
      valid: false,
    };
  }

  const panelChannel = await guild.channels.fetch(panel.channelId).catch(() => null);
  if (!panelChannel || !panelChannel.isTextBased()) {
    return {
      state: 'mismatch (kanal nie istnieje)',
      channel: panelChannel || null,
      message: null,
      valid: false,
    };
  }

  if (!panelChannel.messages || typeof panelChannel.messages.fetch !== 'function') {
    return {
      state: 'mismatch (brak dostepu do wiadomosci)',
      channel: panelChannel,
      message: null,
      valid: false,
    };
  }

  const panelMessage = await panelChannel.messages.fetch(panel.messageId).catch(() => null);
  if (!panelMessage) {
    return {
      state: 'mismatch (wiadomosc nie istnieje)',
      channel: panelChannel,
      message: null,
      valid: false,
    };
  }

  return {
    state: 'ok',
    channel: panelChannel,
    message: panelMessage,
    valid: true,
  };
}

function getMemberHasRole(member, roleId) {
  return Boolean(roleId && member?.roles?.cache?.has?.(roleId));
}

function getChannelPermissionIssue(channel, me) {
  const permissions = typeof channel?.permissionsFor === 'function' ? channel.permissionsFor(me) : null;
  if (!permissions) return null;

  const missing = [];
  if (!permissions.has(PermissionFlagsBits.ViewChannel)) missing.push('View Channel');
  if (!permissions.has(PermissionFlagsBits.SendMessages)) missing.push('Send Messages');
  if (!permissions.has(PermissionFlagsBits.ReadMessageHistory)) missing.push('Read Message History');

  return missing.length > 0 ? missing.join(', ') : null;
}

async function validateManageableVerifyRole(guild, role) {
  const me = await fetchBotMember(guild);
  if (!me) {
    return {
      ok: false,
      error: 'Nie moge sprawdzic uprawnien bota.',
      me: null,
    };
  }

  if (!me.permissions.has(PermissionFlagsBits.ManageRoles)) {
    return {
      ok: false,
      error: 'Bot nie ma uprawnienia Manage Roles.',
      me,
    };
  }

  if (!role || !('position' in role)) {
    return {
      ok: false,
      error: 'Niepoprawna rola.',
      me,
    };
  }

  if (role.position >= me.roles.highest.position) {
    return {
      ok: false,
      error: 'Nie moge nadawac ani odbierac tej roli (jest wyzej lub rowno najwyzszej roli bota).',
      me,
    };
  }

  return {
    ok: true,
    me,
  };
}

async function resolvePanelChannel(interaction, verification) {
  const explicitChannel = interaction.options.getChannel('channel', false);
  if (explicitChannel) {
    return explicitChannel;
  }

  const storedChannelId = verification?.panel?.channelId;
  if (storedChannelId) {
    const storedChannel = await interaction.guild.channels.fetch(storedChannelId).catch(() => null);
    if (storedChannel && storedChannel.isTextBased()) {
      return storedChannel;
    }
  }

  return interaction.channel;
}

async function tryDeleteExistingPanelMessage(guild, panel, { ignoreMessageId = null } = {}) {
  if (!panel?.channelId || !panel?.messageId || panel.messageId === ignoreMessageId) {
    return false;
  }

  const panelChannel = await guild.channels.fetch(panel.channelId).catch(() => null);
  if (!panelChannel?.messages || typeof panelChannel.messages.fetch !== 'function') {
    return false;
  }

  const panelMessage = await panelChannel.messages.fetch(panel.messageId).catch(() => null);
  if (!panelMessage || typeof panelMessage.delete !== 'function') {
    return false;
  }

  try {
    await panelMessage.delete();
    return true;
  } catch {
    return false;
  }
}

function getVerificationStateSafe(stateStore, guildId) {
  const verification = stateStore.getVerificationState(guildId) || {};
  return {
    enabled: verification.enabled === true,
    roleId: verification.roleId ? String(verification.roleId) : null,
    panel: {
      ...VERIFY_PANEL_STATE_EMPTY,
      ...(verification.panel || {}),
    },
    exceptions: dedupeIds(verification.exceptions),
    expirations: verification.expirations && typeof verification.expirations === 'object' ? { ...verification.expirations } : {},
    challenges: verification.challenges && typeof verification.challenges === 'object' ? { ...verification.challenges } : {},
    lockouts: verification.lockouts && typeof verification.lockouts === 'object' ? { ...verification.lockouts } : {},
    lastSweepAt: verification.lastSweepAt || null,
  };
}

async function buildStatusLines(guild, stateStore) {
  const verify = getVerificationStateSafe(stateStore, guild.id);
  const panelInfo = await inspectPanelState(guild, verify.panel);

  return [
    `Enabled: ${verify.enabled ? 'tak' : 'nie'}`,
    `Rola: ${verify.roleId ? `<@&${verify.roleId}>` : 'brak'}`,
    `Panel channel: ${verify.panel.channelId ? `<#${verify.panel.channelId}>` : 'brak'}`,
    `Panel messageId: ${verify.panel.messageId || 'brak'}`,
    `Panel status: ${panelInfo.state}`,
    `Custom ID: ${verify.panel.customId || 'brak'}`,
    `Wyjatki: ${verify.exceptions.length}${verify.exceptions.length > 0 ? ` (${formatUserList(verify.exceptions)})` : ''}`,
    `Aktywne expiracje: ${Object.keys(verify.expirations).length}`,
    `Aktywne challenge: ${Object.keys(verify.challenges).length}`,
    `Aktywne blokady: ${Object.keys(verify.lockouts).length}`,
    `Last sweep: ${verify.lastSweepAt || 'nigdy'}`,
  ];
}

function mutateVerificationState(stateStore, guildId, updater) {
  return stateStore.saveVerificationState(guildId, (verification, currentState) => {
    const safeBase = getVerificationStateSafe(stateStore, guildId);
    const currentVerification = {
      ...safeBase,
      ...(verification || {}),
      panel: {
        ...VERIFY_PANEL_STATE_EMPTY,
        ...(safeBase.panel || {}),
        ...((verification && verification.panel) || {}),
      },
    };

    return updater(currentVerification, currentState);
  });
}

async function sendOrResendPanel(interaction, { stateStore, role, selectedChannel, reason }) {
  const guild = interaction.guild;
  const guildId = guild.id;
  const verifyBefore = getVerificationStateSafe(stateStore, guildId);
  const deferred = await deferEphemeral(interaction);

  const permissionCheck = await validateManageableVerifyRole(guild, role);
  if (!permissionCheck.ok) {
    await finishEphemeral(interaction, deferred, permissionCheck.error);
    return;
  }

  const channel = selectedChannel || interaction.channel;
  if (!channel || !channel.isTextBased()) {
    await finishEphemeral(interaction, deferred, 'Ten kanal nie obsluguje wiadomosci tekstowych.');
    return;
  }

  const channelPermissionIssue = getChannelPermissionIssue(channel, permissionCheck.me);
  if (channelPermissionIssue) {
    await finishEphemeral(interaction, deferred, `Bot nie moze wyslac panelu na ten kanal. Brakuje: ${channelPermissionIssue}.`);
    return;
  }

  let sent;
  try {
    sent = await interaction.client.rest.post(Routes.channelMessages(channel.id), {
      body: buildVerifyPanelPayload(),
    });
  } catch (error) {
    await finishEphemeral(interaction, deferred, `Nie udalo sie wyslac panelu verify: ${error.message}`);
    return;
  }

  const roleChanged = verifyBefore.roleId && verifyBefore.roleId !== role.id;
  mutateVerificationState(stateStore, guildId, (verification) => ({
    enabled: true,
    roleId: role.id,
    panel: createPanelState(channel.id, sent.id, VERIFY_BUTTON_CUSTOM_ID),
    challenges: roleChanged ? {} : verification.challenges,
  }));

  const deletedPreviousPanel = await tryDeleteExistingPanelMessage(guild, verifyBefore.panel, {
    ignoreMessageId: sent.id,
  });

  const followup = [
    reason === 'setup'
      ? `Panel verify utworzony na <#${channel.id}> dla roli <@&${role.id}>.`
      : `Panel verify wyslany ponownie na <#${channel.id}> dla roli <@&${role.id}>.`,
  ];

  if (deletedPreviousPanel) {
    followup.push('Poprzedni panel zostal usuniety, zeby nie zostawiac martwego przycisku.');
  }
  if (roleChanged) {
    followup.push('Aktywne challenge zostaly wyczyszczone, bo zmienila sie rola verify.');
  }

  const statusLines = await buildStatusLines(guild, stateStore);
  await finishEphemeral(interaction, deferred, [...followup, '', ...statusLines].join('\n'));
}

async function runCleanupState(guild, stateStore) {
  const guildId = guild.id;
  const now = new Date();
  const nowIsoValue = now.toISOString();
  const nowTs = now.getTime();

  const verification = getVerificationStateSafe(stateStore, guildId);
  const role = await fetchRole(guild, verification.roleId);
  const members = await fetchAllMembers(guild);
  const memberMap = members instanceof Map
    ? members
    : (members?.cache instanceof Map ? members.cache : members);

  const counts = {
    removedExceptions: 0,
    removedExpirations: 0,
    repairedExpirations: 0,
    removedChallenges: 0,
    removedLockouts: 0,
    removedRoles: 0,
    clearedPanel: false,
  };

  const uniqueExceptions = dedupeIds(verification.exceptions);
  const nextExceptions = [];
  for (const userId of uniqueExceptions) {
    if (memberMap && typeof memberMap.has === 'function' && !memberMap.has(userId)) {
      counts.removedExceptions += 1;
      continue;
    }
    nextExceptions.push(userId);
  }
  const exceptionSet = new Set(nextExceptions);

  const nextChallenges = {};
  for (const [userId, challenge] of Object.entries(verification.challenges)) {
    const expiresAtTs = Date.parse(challenge?.expiresAt || '');
    if (!Number.isFinite(expiresAtTs) || expiresAtTs <= nowTs) {
      counts.removedChallenges += 1;
      continue;
    }
    if (exceptionSet.has(userId)) {
      counts.removedChallenges += 1;
      continue;
    }
    if (memberMap && typeof memberMap.has === 'function' && !memberMap.has(userId)) {
      counts.removedChallenges += 1;
      continue;
    }
    nextChallenges[userId] = challenge;
  }

  const nextLockouts = {};
  for (const [userId, lockoutUntil] of Object.entries(verification.lockouts)) {
    const lockoutTs = Date.parse(lockoutUntil || '');
    if (!Number.isFinite(lockoutTs) || lockoutTs <= nowTs) {
      counts.removedLockouts += 1;
      continue;
    }
    if (exceptionSet.has(userId)) {
      counts.removedLockouts += 1;
      continue;
    }
    if (memberMap && typeof memberMap.has === 'function' && !memberMap.has(userId)) {
      counts.removedLockouts += 1;
      continue;
    }
    nextLockouts[userId] = lockoutUntil;
  }

  const nextExpirations = {};
  for (const [userId, expiryIso] of Object.entries(verification.expirations)) {
    if (exceptionSet.has(userId)) {
      counts.removedExpirations += 1;
      continue;
    }

    const member = memberMap && typeof memberMap.get === 'function' ? memberMap.get(userId) : null;
    if (memberMap && typeof memberMap.has === 'function' && !memberMap.has(userId)) {
      counts.removedExpirations += 1;
      continue;
    }

    if (role && member && !getMemberHasRole(member, role.id)) {
      counts.removedExpirations += 1;
      continue;
    }

    const expiryTs = Date.parse(expiryIso || '');
    if (!Number.isFinite(expiryTs)) {
      counts.removedExpirations += 1;
      continue;
    }

    if (expiryTs <= nowTs) {
      if (role && member && getMemberHasRole(member, role.id) && typeof member.roles?.remove === 'function') {
        try {
          await member.roles.remove(role.id, 'Verify cleanup_state expiry');
          counts.removedRoles += 1;
        } catch {
          // Keep state cleanup moving even if role removal fails.
        }
      }
      counts.removedExpirations += 1;
      continue;
    }

    nextExpirations[userId] = expiryIso;
  }

  if (role && memberMap && typeof memberMap.values === 'function') {
    for (const member of memberMap.values()) {
      if (!getMemberHasRole(member, role.id)) continue;
      if (exceptionSet.has(member.id)) continue;

      const existingExpiryIso = nextExpirations[member.id];
      const existingExpiryTs = Date.parse(existingExpiryIso || '');
      if (existingExpiryIso && Number.isFinite(existingExpiryTs) && existingExpiryTs > nowTs) {
        continue;
      }

      nextExpirations[member.id] = addOneCalendarMonthIso(now);
      counts.repairedExpirations += 1;
    }
  }

  let nextPanel = verification.panel;
  const panelInfo = await inspectPanelState(guild, verification.panel);
  if (verification.panel.channelId && verification.panel.messageId && !panelInfo.valid) {
    nextPanel = { ...VERIFY_PANEL_STATE_EMPTY };
    counts.clearedPanel = true;
  }

  stateStore.saveVerificationState(guildId, {
    panel: nextPanel,
    exceptions: nextExceptions,
    expirations: nextExpirations,
    challenges: nextChallenges,
    lockouts: nextLockouts,
    lastSweepAt: nowIsoValue,
  });

  return counts;
}

async function executeVerifyCommand(interaction, { stateStore }) {
  if (!interaction.inGuild()) {
    await replyEphemeral(interaction, 'Ta komenda dziala tylko na serwerze.');
    return;
  }

  if (!interaction.memberPermissions?.has(PermissionFlagsBits.Administrator)) {
    await replyEphemeral(interaction, 'Brak uprawnien Administratora.');
    return;
  }

  const guild = interaction.guild;
  const guildId = guild.id;
  const mode = String(interaction.options.getString('mode', true) || '').toLowerCase();

  if (mode === 'setup') {
    const role = interaction.options.getRole('rola', false);
    if (!role) {
      await replyEphemeral(interaction, 'Dla mode=setup podaj opcje `rola`.');
      return;
    }
    if (!role || !('position' in role)) {
      await replyEphemeral(interaction, 'Niepoprawna rola.');
      return;
    }

    const selectedChannel = await resolvePanelChannel(interaction, getVerificationStateSafe(stateStore, guildId));
    await sendOrResendPanel(interaction, {
      stateStore,
      role,
      selectedChannel,
      reason: 'setup',
    });
    return;
  }

  if (mode === 'status') {
    const lines = await buildStatusLines(guild, stateStore);
    await replyEphemeral(interaction, lines.join('\n'));
    return;
  }

  if (mode === 'resend_panel') {
    const verification = getVerificationStateSafe(stateStore, guildId);
    if (!verification.roleId) {
      await replyEphemeral(interaction, 'Brak skonfigurowanej roli verify. Najpierw uzyj `/verify mode:setup rola:<rola>`.');
      return;
    }

    const role = await fetchRole(guild, verification.roleId);
    if (!role) {
      await replyEphemeral(interaction, `Skonfigurowana rola verify (\`${verification.roleId}\`) nie istnieje.`);
      return;
    }

    const selectedChannel = await resolvePanelChannel(interaction, verification);
    await sendOrResendPanel(interaction, {
      stateStore,
      role,
      selectedChannel,
      reason: 'resend_panel',
    });
    return;
  }

  if (mode === 'exception_add') {
    const user = interaction.options.getUser('user', false);
    if (!user) {
      await replyEphemeral(interaction, 'Dla mode=exception_add podaj opcje `user`.');
      return;
    }

    const member = await fetchMember(guild, user.id);
    const roleId = getVerificationStateSafe(stateStore, guildId).roleId;

    mutateVerificationState(stateStore, guildId, (verification) => {
      const exceptions = dedupeIds([...(verification.exceptions || []), user.id]);
      const expirations = { ...(verification.expirations || {}) };
      const challenges = { ...(verification.challenges || {}) };
      const lockouts = { ...(verification.lockouts || {}) };

      delete expirations[user.id];
      delete challenges[user.id];
      delete lockouts[user.id];

      return {
        exceptions,
        expirations,
        challenges,
        lockouts,
      };
    });

    const statusLines = await buildStatusLines(guild, stateStore);
    const notes = [
      `Dodano <@${user.id}> do wyjatkow verify.`,
      member && roleId && !getMemberHasRole(member, roleId)
        ? 'Uwaga: uzytkownik nie ma teraz roli verify, wiec wyjatek nie nadaje jej automatycznie.'
        : null,
    ].filter(Boolean);
    await replyEphemeral(interaction, [...notes, '', ...statusLines].join('\n'));
    return;
  }

  if (mode === 'exception_remove') {
    const user = interaction.options.getUser('user', false);
    if (!user) {
      await replyEphemeral(interaction, 'Dla mode=exception_remove podaj opcje `user`.');
      return;
    }

    const verification = getVerificationStateSafe(stateStore, guildId);
    const member = await fetchMember(guild, user.id);
    const hasVerifyRole = Boolean(verification.roleId && member && getMemberHasRole(member, verification.roleId));

    mutateVerificationState(stateStore, guildId, (currentVerification) => {
      const exceptions = dedupeIds(currentVerification.exceptions).filter((id) => id !== user.id);
      const expirations = { ...(currentVerification.expirations || {}) };

      if (hasVerifyRole) {
        expirations[user.id] = addOneCalendarMonthIso(new Date());
      } else {
        delete expirations[user.id];
      }

      return {
        exceptions,
        expirations,
      };
    });

    const statusLines = await buildStatusLines(guild, stateStore);
    const notes = [
      `Usunieto <@${user.id}> z wyjatkow verify.`,
      hasVerifyRole
        ? `Uzytkownik zachowal role verify i dostal nowa expiracje od teraz.`
        : 'Jesli uzytkownik nie ma roli verify, nic nie zostalo mu automatycznie nadane.',
    ];
    await replyEphemeral(interaction, [...notes, '', ...statusLines].join('\n'));
    return;
  }

  if (mode === 'expire_user') {
    const user = interaction.options.getUser('user', false);
    if (!user) {
      await replyEphemeral(interaction, 'Dla mode=expire_user podaj opcje `user`.');
      return;
    }

    const verification = getVerificationStateSafe(stateStore, guildId);
    const member = await fetchMember(guild, user.id);
    const role = await fetchRole(guild, verification.roleId);

    let removedRole = false;
    let removalIssue = null;
    if (role && member && getMemberHasRole(member, role.id)) {
      const permissionCheck = await validateManageableVerifyRole(guild, role);
      if (!permissionCheck.ok) {
        removalIssue = permissionCheck.error;
      } else if (typeof member.roles?.remove === 'function') {
        try {
          await member.roles.remove(role.id, 'Verify expire_user');
          removedRole = true;
        } catch (error) {
          removalIssue = `Nie udalo sie odebrac roli: ${error.message}`;
        }
      }
    }

    mutateVerificationState(stateStore, guildId, (currentVerification) => {
      const expirations = { ...(currentVerification.expirations || {}) };
      const challenges = { ...(currentVerification.challenges || {}) };
      const lockouts = { ...(currentVerification.lockouts || {}) };

      delete expirations[user.id];
      delete challenges[user.id];
      delete lockouts[user.id];

      return {
        expirations,
        challenges,
        lockouts,
      };
    });

    const statusLines = await buildStatusLines(guild, stateStore);
    const notes = [
      `Wygaszono verify dla <@${user.id}>.`,
      removedRole
        ? 'Rola verify zostala odebrana od razu.'
        : (role
          ? 'Stan cache zostal wyczyszczony. Rola nie zostala odebrana, bo uzytkownik jej nie mial albo nie dalo sie tego zrobic teraz.'
          : 'Stan cache zostal wyczyszczony. Skonfigurowana rola verify nie istnieje.'),
      verification.exceptions.includes(user.id)
        ? 'Uwaga: uzytkownik nadal jest na liscie wyjatkow. Jesli ma przechodzic normalny verify, uzyj tez `exception_remove`.'
        : null,
      removalIssue ? `Uwaga: ${removalIssue}` : null,
    ].filter(Boolean);
    await replyEphemeral(interaction, [...notes, '', ...statusLines].join('\n'));
    return;
  }

  if (mode === 'cleanup_state') {
    const counts = await runCleanupState(guild, stateStore);
    const statusLines = await buildStatusLines(guild, stateStore);
    const notes = [
      'Cleanup verify zakonczony.',
      `Usuniete wyjatki poza serwerem: ${counts.removedExceptions}`,
      `Usuniete stare expiracje: ${counts.removedExpirations}`,
      `Naprawione brakujace expiracje: ${counts.repairedExpirations}`,
      `Usuniete stare challenge: ${counts.removedChallenges}`,
      `Usuniete stare blokady: ${counts.removedLockouts}`,
      `Odebrane role po przeterminowaniu: ${counts.removedRoles}`,
      `Panel cleanup: ${counts.clearedPanel ? 'wyczyszczono martwe referencje' : 'bez zmian'}`,
    ];
    await replyEphemeral(interaction, [...notes, '', ...statusLines].join('\n'));
    return;
  }

  await replyEphemeral(interaction, `Nieznany mode verify: ${mode}.`);
}

module.exports = {
  verifyCommand,
  executeVerifyCommand,
  VERIFY_BUTTON_CUSTOM_ID,
};
