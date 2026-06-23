const {
  ApplicationCommandOptionType,
  PermissionFlagsBits,
} = require('discord.js');
const packageJson = require('../../package.json');
const { resolveOwnerIds } = require('../core/owners');
const { resolveOperatorPaths } = require('../core/paths');

const COMPONENTS_V2_FLAG = 32768;
const EPHEMERAL_FLAG = 64;
const TARGET_NODE_MAJOR = 22;

const healthCommand = {
  name: 'health',
  description: 'Szybki stan bota i kluczowych modulow',
  dm_permission: false,
  default_member_permissions: PermissionFlagsBits.Administrator.toString(),
};

const debugCommand = {
  name: 'debug',
  description: 'Diagnostyka muzyki, uprawnien i logow',
  dm_permission: false,
  default_member_permissions: PermissionFlagsBits.Administrator.toString(),
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Zakres diagnostyki',
      required: true,
      choices: [
        { name: 'music', value: 'music' },
        { name: 'permissions', value: 'permissions' },
        { name: 'logs', value: 'logs' },
      ],
    },
    {
      type: ApplicationCommandOptionType.Channel,
      name: 'channel',
      description: 'Opcjonalny kanal do sprawdzenia uprawnien',
      required: false,
    },
  ],
};

const GUILD_PERMISSION_CHECKS = [
  { bit: PermissionFlagsBits.ViewAuditLog, label: 'View Audit Log' },
  { bit: PermissionFlagsBits.ManageRoles, label: 'Manage Roles' },
  { bit: PermissionFlagsBits.ModerateMembers, label: 'Moderate Members' },
];

const TEXT_PERMISSION_CHECKS = [
  { bit: PermissionFlagsBits.ViewChannel, label: 'View Channel' },
  { bit: PermissionFlagsBits.SendMessages, label: 'Send Messages' },
  { bit: PermissionFlagsBits.ReadMessageHistory, label: 'Read Message History' },
];

const VOICE_PERMISSION_CHECKS = [
  { bit: PermissionFlagsBits.ViewChannel, label: 'View Channel' },
  { bit: PermissionFlagsBits.Connect, label: 'Connect' },
  { bit: PermissionFlagsBits.Speak, label: 'Speak' },
];

function truncateInline(value, maxLength = 180) {
  const text = String(value ?? '').replace(/\s+/g, ' ').trim();
  if (!text) return '';
  if (text.length <= maxLength) return text;
  return `${text.slice(0, Math.max(0, maxLength - 3)).trimEnd()}...`;
}

function toSeverityRank(level) {
  if (level === 'critical') return 2;
  if (level === 'warning') return 1;
  return 0;
}

function getOverallSeverity(issues) {
  let rank = 0;
  for (const issue of issues) {
    rank = Math.max(rank, toSeverityRank(issue.severity));
  }
  if (rank >= 2) return 'critical';
  if (rank === 1) return 'warning';
  return 'ok';
}

function severityBadge(level) {
  if (level === 'critical') return '❌ Krytyczne';
  if (level === 'warning') return '⚠️ Uwaga';
  return '✅ OK';
}

function buildIssue(severity, message) {
  return {
    severity,
    message: truncateInline(message, 220),
  };
}

function formatChannelMention(channelLike, fallbackName = 'kanal') {
  if (!channelLike) return fallbackName;
  if (typeof channelLike === 'string') {
    return /^\d{15,22}$/.test(channelLike) ? `<#${channelLike}>` : truncateInline(channelLike, 80);
  }
  if (channelLike.id && /^\d{15,22}$/.test(String(channelLike.id))) {
    return `<#${channelLike.id}>`;
  }
  if (typeof channelLike.toString === 'function') {
    const rendered = String(channelLike);
    if (rendered && rendered !== '[object Object]') return rendered;
  }
  return truncateInline(channelLike.name || fallbackName, 80);
}

function formatBool(value, yes = 'tak', no = 'nie') {
  return value ? yes : no;
}

function formatNodeRuntime(node) {
  const status = node.connected ? 'online' : 'offline';
  const load = node.cpuLoad > 0 ? `${Math.round(node.cpuLoad * 100)}% cpu` : 'cpu ?';
  return `${node.id} ${node.host}:${node.port} | ${status} | players ${node.playingPlayers}/${node.players} | ${load}`;
}

function renderPermissionStatus(snapshot) {
  if (!snapshot) return 'brak danych';
  if (!snapshot.available) return snapshot.reason || 'brak danych';
  if (snapshot.missing.length === 0) return `OK (${snapshot.ok.join(', ') || 'required permissions present'})`;
  return `Brakuje: ${snapshot.missing.join(', ')}`;
}

function checkPermissions(permissionHolder, checks) {
  if (!permissionHolder || typeof permissionHolder.has !== 'function') {
    return {
      available: false,
      ok: [],
      missing: checks.map((entry) => entry.label),
      reason: 'nie mozna odczytac uprawnien',
    };
  }

  const ok = [];
  const missing = [];
  for (const entry of checks) {
    if (permissionHolder.has(entry.bit)) {
      ok.push(entry.label);
    } else {
      missing.push(entry.label);
    }
  }

  return {
    available: true,
    ok,
    missing,
  };
}

function inspectChannelPermissions(channel, member, checks) {
  if (!channel) {
    return {
      available: false,
      ok: [],
      missing: checks.map((entry) => entry.label),
      reason: 'kanal niedostepny',
    };
  }
  if (!member) {
    return {
      available: false,
      ok: [],
      missing: checks.map((entry) => entry.label),
      reason: 'bot member niedostepny',
    };
  }

  const perms = typeof channel.permissionsFor === 'function'
    ? channel.permissionsFor(member)
    : null;

  return checkPermissions(perms, checks);
}

async function getBotMember(guild) {
  if (!guild) return null;
  if (guild.members?.me) return guild.members.me;
  if (guild.members?.fetchMe) {
    return guild.members.fetchMe().catch(() => null);
  }
  return null;
}

async function fetchChannelById(guild, channelId) {
  if (!guild || !channelId) return null;
  if (guild.channels?.cache?.get) {
    const cached = guild.channels.cache.get(channelId);
    if (cached) return cached;
  }
  if (guild.channels?.fetch) {
    return guild.channels.fetch(channelId).catch(() => null);
  }
  return null;
}

function getVoiceReference(interaction, musicDiagnostics, explicitChannel = null) {
  if (explicitChannel && typeof explicitChannel.isVoiceBased === 'function' && explicitChannel.isVoiceBased()) {
    return explicitChannel;
  }

  const activeVoiceId = musicDiagnostics?.guildSession?.voiceChannelId || null;
  if (activeVoiceId) {
    return { id: activeVoiceId };
  }

  return interaction.member?.voice?.channel || null;
}

function buildPanel(title, sections) {
  return {
    flags: COMPONENTS_V2_FLAG | EPHEMERAL_FLAG,
    allowed_mentions: {
      parse: [],
    },
    components: [
      {
        type: 17,
        components: [
          {
            type: 10,
            content: `## ${title}`,
          },
          ...sections.map((section) => ({
            type: 10,
            content: [
              `### ${section.title}`,
              ...section.lines.filter(Boolean).map((line) => truncateInline(line, 1800)),
            ].join('\n'),
          })),
        ],
      },
    ],
  };
}

async function inspectMusic(musicManager, guildId) {
  const diagnostics = typeof musicManager?.getDiagnostics === 'function'
    ? musicManager.getDiagnostics(guildId)
    : {
      initialized: false,
      usable: false,
      connectedNodes: 0,
      nodes: [],
      configuredNodes: [],
      config: {},
      recoverySources: [],
      defaultSearchSource: 'unknown',
      defaultVolume: 0,
      queueStateDirExists: false,
      guildSession: null,
    };

  const issues = [];
  if (!diagnostics.initialized || !diagnostics.usable || diagnostics.connectedNodes === 0) {
    issues.push(buildIssue('critical', 'Lavalink jest offline. /music play nie zadziala, dopoki node nie wstanie.'));
  }
  if (!diagnostics.config.hasExplicitPassword) {
    issues.push(buildIssue('warning', 'LAVALINK_PASSWORD nie jest ustawiony jawnie. Bot leci na fallbacku.'));
  }
  if (diagnostics.guildSession?.voiceChannelId && diagnostics.guildSession.playerConnected !== true) {
    issues.push(buildIssue('warning', 'Bot ma aktywna sesje music, ale player nie wyglada na polaczony z voice.'));
  }
  if (!diagnostics.queueStateDirExists) {
    issues.push(buildIssue('warning', 'Katalog cache kolejki nie istnieje. Restore queue nie zapisze stanu, dopoki nie powstanie.'));
  }

  const session = diagnostics.guildSession;
  const voiceSummary = session?.voiceChannelId
    ? `${formatChannelMention(session.voiceChannelId)} | ${session.playing ? 'gra' : (session.paused ? 'pauza' : 'idle')}`
    : 'bot nie siedzi teraz na voice';

  const lines = [
    `Lavalink: ${diagnostics.usable ? 'online' : 'offline'} (${diagnostics.connectedNodes}/${diagnostics.nodes.length || diagnostics.configuredNodes.length || 0} nodes connected)`,
    `Search source: ${diagnostics.defaultSearchSource} | Volume: ${diagnostics.defaultVolume}%`,
    `Recovery: ${diagnostics.recoverySources.length > 0 ? diagnostics.recoverySources.join(', ') : 'brak'}`,
    `Voice: ${voiceSummary}`,
    session?.currentTrackTitle
      ? `Aktualny utwor: ${truncateInline(session.currentTrackTitle, 120)} | kolejka: ${session.queueSize}`
      : 'Aktualny utwor: brak',
    `Voice status na kanale: ${session?.statusText || session?.cachedStatusText || 'brak'}`,
    `Saved queue: ${formatBool(session?.hasSavedQueue === true)}`,
    `Config source: ${diagnostics.config.usesUrl ? 'LAVALINK_URL' : 'HOST/PORT env lub fallback'}`,
    `Config vars: host=${diagnostics.config.hasExplicitHost ? 'set' : 'default'} | port=${diagnostics.config.hasExplicitPort ? 'set' : 'default'} | password=${diagnostics.config.hasExplicitPassword ? 'set' : 'default'} | secure=${diagnostics.config.hasExplicitSecure ? 'set' : 'default'}`,
    diagnostics.nodes.length > 0
      ? `Nodes: ${diagnostics.nodes.slice(0, 3).map(formatNodeRuntime).join(' || ')}`
      : `Nodes: ${diagnostics.configuredNodes.map((node) => `${node.id} ${node.host}:${node.port}`).join(' || ') || 'brak'}`,
  ];

  return {
    diagnostics,
    issues,
    lines,
  };
}

async function inspectLogs(interaction, fullLogs) {
  const diagnostics = typeof fullLogs?.getDiagnostics === 'function'
    ? fullLogs.getDiagnostics(interaction.guildId, interaction.guild)
    : {
      started: false,
      logsDir: 'unknown',
      logsDirExists: false,
      state: {
        enabled: false,
        channelId: null,
        retentionDays: 30,
        includeBots: false,
        forwardMode: 'all',
      },
      canViewAuditLog: false,
      bufferedWrites: 0,
      deliveryQueueSize: 0,
      writeQueueSize: 0,
      auditCacheEntries: 0,
      auditFetchInflight: 0,
      retentionTimerActive: false,
      retentionSweepActive: false,
    };

  const state = diagnostics.state || {};
  const logChannel = state.channelId
    ? await fetchChannelById(interaction.guild, state.channelId)
    : null;
  const me = await getBotMember(interaction.guild);
  const logChannelPermissions = state.channelId
    ? inspectChannelPermissions(logChannel, me, TEXT_PERMISSION_CHECKS)
    : null;

  const issues = [];
  if (!diagnostics.started) {
    issues.push(buildIssue('critical', 'FullLogs module nie jest uruchomiony.'));
  }
  if (!state.enabled) {
    issues.push(buildIssue('warning', 'Logi sa wylaczone na tym serwerze.'));
  }
  if (state.enabled && !state.channelId) {
    issues.push(buildIssue('critical', 'Logi sa wlaczone, ale kanal logow nie jest ustawiony.'));
  }
  if (state.enabled && state.channelId && !logChannel) {
    issues.push(buildIssue('critical', 'Ustawiony kanal logow nie istnieje albo bot nie moze go pobrac.'));
  }
  if (state.enabled && logChannel && !logChannel.isTextBased?.()) {
    issues.push(buildIssue('critical', 'Kanal logow istnieje, ale nie obsluguje wiadomosci tekstowych.'));
  }
  if (state.enabled && logChannelPermissions?.missing?.length > 0) {
    issues.push(buildIssue('critical', `Bot nie moze wysylac logow do ${formatChannelMention(logChannel)}. Brakuje: ${logChannelPermissions.missing.join(', ')}.`));
  }
  if (state.enabled && diagnostics.canViewAuditLog === false) {
    issues.push(buildIssue('warning', 'Brak View Audit Log. W logach moderacyjnych aktor bedzie czesto unknown.'));
  }
  if (!diagnostics.logsDirExists) {
    issues.push(buildIssue('warning', 'Katalog plikow logow nie istnieje. Powstanie dopiero przy pierwszym zapisie albo brakuje dostepu do dysku.'));
  }

  const lines = [
    `Runtime: ${diagnostics.started ? 'uruchomiony' : 'zatrzymany'}`,
    `Stan: ${state.enabled ? 'wlaczone' : 'wylaczone'} | kanal: ${state.channelId ? formatChannelMention(logChannel || state.channelId) : 'brak'}`,
    state.channelId ? `Kanal logow perms: ${renderPermissionStatus(logChannelPermissions)}` : 'Kanal logow perms: brak (kanal nieustawiony)',
    `Audit log access: ${diagnostics.canViewAuditLog ? 'tak' : 'nie'}`,
    `Retention: ${state.retentionDays || 30} dni | Include bots: ${formatBool(state.includeBots === true)} | Forward: ${state.forwardMode || 'all'}`,
    `Kolejki: delivery=${diagnostics.deliveryQueueSize} | write=${diagnostics.writeQueueSize} | buffered=${diagnostics.bufferedWrites}`,
    `Audit cache: cached=${diagnostics.auditCacheEntries} | inflight=${diagnostics.auditFetchInflight}`,
    `Dysk: ${diagnostics.logsDirExists ? 'OK' : 'brak katalogu'} | ${diagnostics.logsDir}`,
    `Retention worker: timer=${formatBool(diagnostics.retentionTimerActive)} | active sweep=${formatBool(diagnostics.retentionSweepActive)}`,
  ];

  return {
    diagnostics,
    state,
    logChannel,
    logChannelPermissions,
    issues,
    lines,
  };
}

async function inspectPermissions(interaction, fullLogs, musicSnapshot, pickedChannel = null) {
  const me = await getBotMember(interaction.guild);
  const targetChannel = pickedChannel || interaction.channel || null;
  const logsState = typeof fullLogs?.getLogsState === 'function'
    ? fullLogs.getLogsState(interaction.guildId)
    : { enabled: false, channelId: null };
  const logChannel = logsState.channelId
    ? await fetchChannelById(interaction.guild, logsState.channelId)
    : null;

  const guildPermissionSnapshot = checkPermissions(me?.permissions || null, GUILD_PERMISSION_CHECKS);
  const targetChecks = typeof targetChannel?.isVoiceBased === 'function' && targetChannel.isVoiceBased()
    ? VOICE_PERMISSION_CHECKS
    : TEXT_PERMISSION_CHECKS;
  const targetPermissionSnapshot = inspectChannelPermissions(targetChannel, me, targetChecks);

  let voiceChannel = getVoiceReference(interaction, musicSnapshot?.diagnostics || {}, pickedChannel);
  if (voiceChannel?.id && !voiceChannel.permissionsFor) {
    voiceChannel = await fetchChannelById(interaction.guild, voiceChannel.id);
  }

  const voicePermissionSnapshot = voiceChannel
    ? inspectChannelPermissions(voiceChannel, me, VOICE_PERMISSION_CHECKS)
    : null;
  const logPermissionSnapshot = logsState.channelId
    ? inspectChannelPermissions(logChannel, me, TEXT_PERMISSION_CHECKS)
    : null;

  const issues = [];
  if (targetPermissionSnapshot.missing.length > 0) {
    issues.push(buildIssue('warning', `Na kanale ${formatChannelMention(targetChannel, 'aktualny kanal')} brakuje: ${targetPermissionSnapshot.missing.join(', ')}.`));
  }
  if (logsState.enabled && logPermissionSnapshot?.missing?.length > 0) {
    issues.push(buildIssue('critical', `Na kanale logow ${formatChannelMention(logChannel || logsState.channelId)} brakuje: ${logPermissionSnapshot.missing.join(', ')}.`));
  }
  if (voiceChannel && voicePermissionSnapshot?.missing?.length > 0) {
    issues.push(buildIssue('critical', `Na voice ${formatChannelMention(voiceChannel)} brakuje: ${voicePermissionSnapshot.missing.join(', ')}.`));
  }
  if (logsState.enabled && guildPermissionSnapshot.missing.includes('View Audit Log')) {
    issues.push(buildIssue('warning', 'Guild-wide brakuje View Audit Log, wiec logi moderacyjne beda niepelne.'));
  }
  if (guildPermissionSnapshot.missing.includes('Manage Roles')) {
    issues.push(buildIssue('warning', 'Guild-wide brakuje Manage Roles, wiec verify i role automation beda ograniczone.'));
  }

  const lines = [
    `Guild-wide: ${renderPermissionStatus(guildPermissionSnapshot)}`,
    `Kanal diagnostyki ${formatChannelMention(targetChannel, 'aktualny kanal')}: ${renderPermissionStatus(targetPermissionSnapshot)}`,
    logsState.channelId
      ? `Kanal logow ${formatChannelMention(logChannel || logsState.channelId)}: ${renderPermissionStatus(logPermissionSnapshot)}`
      : 'Kanal logow: brak konfiguracji',
    voiceChannel
      ? `Voice ${formatChannelMention(voiceChannel)}: ${renderPermissionStatus(voicePermissionSnapshot)}`
      : 'Voice: brak aktywnego kanalu do sprawdzenia',
  ];

  return {
    issues,
    lines,
  };
}

function buildIssueLines(issues) {
  if (issues.length === 0) {
    return ['Brak czerwonych flag.'];
  }

  return issues.map((issue) => `${severityBadge(issue.severity)} ${issue.message}`);
}

function buildRuntimeLines(configPath) {
  const ownerAccess = resolveOwnerIds({ configPath });
  const runtimePaths = resolveOperatorPaths();
  const nodeVersion = process.versions.node;
  const nodeMajor = Number(nodeVersion.split('.')[0]);
  const lines = [
    'Discord gateway: online',
    `Node.js: ${nodeVersion}${nodeMajor === TARGET_NODE_MAJOR ? '' : ` (target ${TARGET_NODE_MAJOR}.x)`}`,
    `discord.js: ${packageJson.dependencies['discord.js']} | lavalink-client: ${packageJson.dependencies['lavalink-client']}`,
    `Owner access: ${ownerAccess.ids.size > 0 ? `${ownerAccess.ids.size} IDs from ${ownerAccess.source}` : 'brak konfiguracji owner IDs'}`,
    `Config: ${configPath}`,
    `Data dir: ${runtimePaths.dataDir}`,
    `Logs dir: ${runtimePaths.logsDir}`,
    `Sync komend na starcie: ${runtimePaths.syncCommandsOnStartup ? 'tak' : 'nie'}`,
  ];

  const issues = [];
  if (nodeMajor !== TARGET_NODE_MAJOR) {
    issues.push(buildIssue('warning', `Runtime jest na Node ${nodeVersion}, a projekt celuje w Node ${TARGET_NODE_MAJOR}.x.`));
  }
  if (ownerAccess.ids.size === 0) {
    issues.push(buildIssue('warning', 'Brak OWNER_IDS. Komendy owner-only beda niedostepne.'));
  }
  if (ownerAccess.invalid.length > 0) {
    issues.push(buildIssue('warning', `Pominiete niepoprawne OWNER_IDS: ${ownerAccess.invalid.join(', ')}.`));
  }

  return { lines, issues };
}

async function executeHealthCommand(interaction, { musicManager, fullLogs, configPath }) {
  if (!interaction.inGuild()) {
    await interaction.reply(buildPanel('Health', [{
      title: 'Info',
      lines: ['Ta komenda dziala tylko na serwerze.'],
    }]));
    return;
  }

  const musicSnapshot = await inspectMusic(musicManager, interaction.guildId);
  const logsSnapshot = await inspectLogs(interaction, fullLogs);
  const permissionsSnapshot = await inspectPermissions(interaction, fullLogs, musicSnapshot);
  const runtimeSnapshot = buildRuntimeLines(configPath);

  const issues = [
    ...runtimeSnapshot.issues,
    ...musicSnapshot.issues,
    ...logsSnapshot.issues,
    ...permissionsSnapshot.issues,
  ];

  const overall = getOverallSeverity(issues);

  const sections = [
    {
      title: 'Podsumowanie',
      lines: [
        `Stan ogolny: ${severityBadge(overall)}`,
        overall === 'ok'
          ? 'Bot wyglada zdrowo. Kluczowe moduly sa gotowe do pracy.'
          : 'Ponizej masz najwazniejsze problemy do sprawdzenia przed dalsza diagnostyka.',
      ],
    },
    {
      title: 'Runtime',
      lines: runtimeSnapshot.lines.map((line, index) => (index === 0
        ? `Discord: online jako ${interaction.client?.user?.tag || 'unknown'}`
        : line)),
    },
    {
      title: 'Music',
      lines: musicSnapshot.lines.slice(0, 5),
    },
    {
      title: 'Logs',
      lines: logsSnapshot.lines.slice(0, 4),
    },
    {
      title: 'Red Flags',
      lines: buildIssueLines(issues),
    },
  ];

  await interaction.reply(buildPanel('Health', sections));
}

async function executeDebugCommand(interaction, { musicManager, fullLogs, configPath }) {
  if (!interaction.inGuild()) {
    await interaction.reply(buildPanel('Debug', [{
      title: 'Info',
      lines: ['Ta komenda dziala tylko na serwerze.'],
    }]));
    return;
  }

  const mode = String(interaction.options.getString('mode', true) || '').toLowerCase();
  const pickedChannel = interaction.options.getChannel('channel', false) || null;

  if (mode === 'music') {
    const musicSnapshot = await inspectMusic(musicManager, interaction.guildId);
    const runtimeSnapshot = buildRuntimeLines(configPath);
    await interaction.reply(buildPanel('Debug Music', [
      {
        title: 'Stan',
        lines: [
          `Ocena: ${severityBadge(getOverallSeverity([...runtimeSnapshot.issues, ...musicSnapshot.issues]))}`,
          ...musicSnapshot.lines,
        ],
      },
      {
        title: 'Red Flags',
        lines: buildIssueLines([...runtimeSnapshot.issues, ...musicSnapshot.issues]),
      },
    ]));
    return;
  }

  if (mode === 'permissions') {
    const musicSnapshot = await inspectMusic(musicManager, interaction.guildId);
    const permissionsSnapshot = await inspectPermissions(interaction, fullLogs, musicSnapshot, pickedChannel);
    await interaction.reply(buildPanel('Debug Permissions', [
      {
        title: 'Stan',
        lines: permissionsSnapshot.lines,
      },
      {
        title: 'Red Flags',
        lines: buildIssueLines(permissionsSnapshot.issues),
      },
    ]));
    return;
  }

  if (mode === 'logs') {
    const logsSnapshot = await inspectLogs(interaction, fullLogs);
    await interaction.reply(buildPanel('Debug Logs', [
      {
        title: 'Stan',
        lines: logsSnapshot.lines,
      },
      {
        title: 'Red Flags',
        lines: buildIssueLines(logsSnapshot.issues),
      },
    ]));
    return;
  }

  await interaction.reply(buildPanel('Debug', [{
    title: 'Info',
    lines: [`Nieznany mode debug: ${mode}.`],
  }]));
}

module.exports = {
  debugCommand,
  executeDebugCommand,
  executeHealthCommand,
  healthCommand,
};
