const {
  ApplicationCommandOptionType,
  AttachmentBuilder,
  ChannelType,
  PermissionFlagsBits,
} = require('discord.js');
const FullLogs = require('../events/full-logs');

const COMPONENTS_V2_FLAG = 32768;
const EPHEMERAL_FLAG = 64;
const DEFAULT_RETENTION_DAYS = 30;
const MIN_RETENTION_DAYS = 1;
const MAX_RETENTION_DAYS = 365;
const LOG_CATEGORY_KEYS = Array.isArray(FullLogs.LOG_CATEGORY_KEYS) ? FullLogs.LOG_CATEGORY_KEYS : [];
const CATEGORY_META = FullLogs.CATEGORY_META || {};
const CATEGORY_CHOICES = [
  { name: 'all', value: 'all' },
  ...LOG_CATEGORY_KEYS.map((key) => ({
    name: `${CATEGORY_META[key]?.label || key}`,
    value: key,
  })),
];

const logsCommand = {
  name: 'logs',
  description: 'Pelne logi serwera i konfiguracja kanalu logow',
  dm_permission: false,
  default_member_permissions: PermissionFlagsBits.Administrator.toString(),
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Tryb komendy logs',
      required: true,
      choices: [
        { name: 'setup', value: 'setup' },
        { name: 'status', value: 'status' },
        { name: 'enable', value: 'enable' },
        { name: 'disable', value: 'disable' },
        { name: 'channel', value: 'channel' },
        { name: 'retention', value: 'retention' },
        { name: 'include_bots', value: 'include_bots' },
        { name: 'test', value: 'test' },
        { name: 'preview', value: 'preview' },
        { name: 'export_day', value: 'export_day' },
        { name: 'categories', value: 'categories' },
        { name: 'purge', value: 'purge' },
      ],
    },
    {
      type: ApplicationCommandOptionType.Channel,
      name: 'channel',
      description: 'Kanal logow (dla setup/channel)',
      required: false,
      channel_types: [ChannelType.GuildText, ChannelType.GuildAnnouncement],
    },
    {
      type: ApplicationCommandOptionType.Integer,
      name: 'retention_days',
      description: `Ile dni trzymac pliki logow (${MIN_RETENTION_DAYS}-${MAX_RETENTION_DAYS})`,
      required: false,
      min_value: MIN_RETENTION_DAYS,
      max_value: MAX_RETENTION_DAYS,
    },
    {
      type: ApplicationCommandOptionType.Boolean,
      name: 'value',
      description: 'Wartosc true/false dla include_bots albo categories',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'day',
      description: 'Dzien logu w formacie YYYY-MM-DD',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'category',
      description: 'Kategoria do wlaczenia lub wylaczenia',
      required: false,
      choices: CATEGORY_CHOICES.slice(0, 25),
    },
    {
      type: ApplicationCommandOptionType.Integer,
      name: 'older_than_days',
      description: 'Purge plikow starszych niz X dni',
      required: false,
      min_value: 1,
      max_value: 3650,
    },
    {
      type: ApplicationCommandOptionType.Boolean,
      name: 'confirm',
      description: 'Potwierdza purge logow',
      required: false,
    },
  ],
};

function normalizeRetention(value, fallback = DEFAULT_RETENTION_DAYS) {
  const raw = Number(value);
  if (!Number.isFinite(raw)) return fallback;
  const rounded = Math.round(raw);
  return Math.min(MAX_RETENTION_DAYS, Math.max(MIN_RETENTION_DAYS, rounded));
}

function truncateInline(value, maxLength = 180) {
  const text = String(value ?? '').replace(/\s+/g, ' ').trim();
  if (!text) return '';
  if (text.length <= maxLength) return text;
  return `${text.slice(0, Math.max(0, maxLength - 3)).trimEnd()}...`;
}

function buildPanel(title, lines) {
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
          {
            type: 10,
            content: lines.join('\n'),
          },
        ],
      },
    ],
  };
}

function normalizeDisabledCategories(values) {
  const normalized = Array.isArray(values)
    ? values.map((value) => String(value || '').trim().toLowerCase()).filter((value) => LOG_CATEGORY_KEYS.includes(value))
    : [];
  return [...new Set(normalized)].sort();
}

function resolveLogsState(stateStore, guildId) {
  const state = stateStore.getLogsState(guildId);
  return {
    enabled: state.enabled === true,
    channelId: state.channelId ? String(state.channelId) : null,
    retentionDays: normalizeRetention(state.retentionDays, DEFAULT_RETENTION_DAYS),
    forwardMode: String(state.forwardMode || 'all'),
    includeBots: state.includeBots === true,
    disabledCategories: normalizeDisabledCategories(state.disabledCategories),
  };
}

function getEnabledCategories(logsState) {
  const disabled = new Set(normalizeDisabledCategories(logsState?.disabledCategories));
  return LOG_CATEGORY_KEYS.filter((key) => !disabled.has(key));
}

function formatCategoryKey(key) {
  const meta = CATEGORY_META[key] || {};
  return `${meta.icon || '•'} ${meta.label || key}`;
}

function formatCategoryList(keys, { empty = 'brak' } = {}) {
  if (!Array.isArray(keys) || keys.length === 0) {
    return empty;
  }
  return keys.map((key) => formatCategoryKey(key)).join(', ');
}

function buildStatusLines(logsState, channelStatus = null) {
  const enabledCategories = getEnabledCategories(logsState);
  const disabledCategories = normalizeDisabledCategories(logsState.disabledCategories);

  return [
    `Enabled: **${logsState.enabled ? 'yes' : 'no'}**`,
    `Channel: ${logsState.channelId ? `<#${logsState.channelId}> (\`${logsState.channelId}\`)` : 'not set'}`,
    channelStatus ? `Channel status: ${channelStatus}` : null,
    `Retention: **${logsState.retentionDays} days**`,
    `Include bots: **${logsState.includeBots ? 'yes' : 'no'}**`,
    `Forward mode: **${logsState.forwardMode || 'all'}**`,
    `Enabled categories (${enabledCategories.length}/${LOG_CATEGORY_KEYS.length || enabledCategories.length}): ${formatCategoryList(enabledCategories, { empty: 'brak' })}`,
    `Disabled categories: ${formatCategoryList(disabledCategories, { empty: 'none' })}`,
    '',
    'Modes:',
    '`/logs mode:setup channel:#kanal`',
    '`/logs mode:status`',
    '`/logs mode:enable`',
    '`/logs mode:disable`',
    '`/logs mode:channel channel:#kanal`',
    '`/logs mode:retention retention_days:30`',
    '`/logs mode:include_bots value:true|false`',
    '`/logs mode:categories`',
    '`/logs mode:categories category:message value:false`',
    '`/logs mode:preview`',
    '`/logs mode:export_day day:2026-06-02`',
    '`/logs mode:purge day:2026-06-02 confirm:true`',
    '`/logs mode:purge older_than_days:30 confirm:true`',
    '`/logs mode:test`',
  ].filter(Boolean);
}

function buildCategoriesLines(logsState) {
  const enabledCategories = getEnabledCategories(logsState);
  const disabledCategories = normalizeDisabledCategories(logsState.disabledCategories);

  return [
    `Enabled (${enabledCategories.length}/${LOG_CATEGORY_KEYS.length || enabledCategories.length}): ${formatCategoryList(enabledCategories, { empty: 'brak' })}`,
    `Disabled: ${formatCategoryList(disabledCategories, { empty: 'none' })}`,
    '',
    'Przyklady:',
    '`/logs mode:categories category:message value:false`',
    '`/logs mode:categories category:voice value:true`',
    '`/logs mode:categories category:all value:true`',
  ];
}

function isValidDayKey(value) {
  const normalized = String(value || '').trim();
  if (!/^\d{4}-\d{2}-\d{2}$/.test(normalized)) return false;
  const timestamp = Date.parse(`${normalized}T00:00:00.000Z`);
  if (!Number.isFinite(timestamp)) return false;
  return new Date(timestamp).toISOString().slice(0, 10) === normalized;
}

async function executeLogsCommand(interaction, { stateStore, fullLogs }) {
  if (!interaction.inGuild()) {
    await interaction.reply(buildPanel('Logs', [
      'Ta komenda dziala tylko na serwerze.',
    ]));
    return;
  }

  if (!interaction.memberPermissions?.has(PermissionFlagsBits.Administrator)) {
    await interaction.reply(buildPanel('Logs', [
      'Brak uprawnien Administratora.',
    ]));
    return;
  }

  const guildId = interaction.guildId;
  const mode = String(interaction.options.getString('mode', true) || '').toLowerCase();
  const current = resolveLogsState(stateStore, guildId);

  if (mode === 'setup') {
    const picked = interaction.options.getChannel('channel', false) || interaction.channel;
    if (!picked || !picked.isTextBased()) {
      await interaction.reply(buildPanel('Logs Setup', [
        'Wybrany kanal nie obsluguje wiadomosci tekstowych.',
      ]));
      return;
    }

    stateStore.saveLogsState(guildId, {
      enabled: true,
      channelId: picked.id,
      retentionDays: normalizeRetention(
        interaction.options.getInteger('retention_days', false),
        current.retentionDays,
      ),
      includeBots: interaction.options.getBoolean('value', false) ?? current.includeBots,
      forwardMode: current.forwardMode || 'all',
      disabledCategories: current.disabledCategories,
    });

    const next = resolveLogsState(stateStore, guildId);
    await interaction.reply(buildPanel('Logs Setup', [
      `Logs enabled on ${next.channelId ? `<#${next.channelId}>` : 'unknown channel'}.`,
      `Retention: ${next.retentionDays} days`,
      `Include bots: ${next.includeBots ? 'yes' : 'no'}`,
      `Categories: ${next.disabledCategories.length === 0 ? 'all enabled' : `${getEnabledCategories(next).length} enabled / ${next.disabledCategories.length} disabled`}`,
      'Run `/logs mode:preview` or `/logs mode:test` to verify output.',
    ]));
    return;
  }

  if (mode === 'status') {
    let channelStatus = null;
    if (current.channelId) {
      const channel = await interaction.guild.channels.fetch(current.channelId).catch(() => null);
      if (!channel) {
        channelStatus = 'missing';
      } else if (!channel.isTextBased()) {
        channelStatus = 'channel exists, but not text based';
      } else {
        channelStatus = 'ok';
      }
    }

    await interaction.reply(buildPanel('Logs Status', buildStatusLines(current, channelStatus)));
    return;
  }

  if (mode === 'enable') {
    stateStore.saveLogsState(guildId, {
      enabled: true,
    });
    const next = resolveLogsState(stateStore, guildId);
    await interaction.reply(buildPanel('Logs Enabled', [
      'Enabled: **yes**',
      `Channel: ${next.channelId ? `<#${next.channelId}>` : 'not set (set with `/logs mode:channel`)'}`,
    ]));
    return;
  }

  if (mode === 'disable') {
    stateStore.saveLogsState(guildId, {
      enabled: false,
    });
    await interaction.reply(buildPanel('Logs Disabled', [
      'Enabled: **no**',
      'Event tracking to channel has been paused.',
    ]));
    return;
  }

  if (mode === 'channel') {
    const picked = interaction.options.getChannel('channel', false) || interaction.channel;
    if (!picked || !picked.isTextBased()) {
      await interaction.reply(buildPanel('Logs Channel', [
        'Wybrany kanal nie obsluguje wiadomosci tekstowych.',
      ]));
      return;
    }

    stateStore.saveLogsState(guildId, {
      channelId: picked.id,
    });

    await interaction.reply(buildPanel('Logs Channel', [
      `Log channel set to ${picked}.`,
    ]));
    return;
  }

  if (mode === 'retention') {
    const picked = interaction.options.getInteger('retention_days', false);
    if (!Number.isFinite(picked)) {
      await interaction.reply(buildPanel('Logs Retention', [
        `Podaj \`retention_days\` (${MIN_RETENTION_DAYS}-${MAX_RETENTION_DAYS}).`,
      ]));
      return;
    }

    const retentionDays = normalizeRetention(picked, current.retentionDays);
    stateStore.saveLogsState(guildId, {
      retentionDays,
    });

    await interaction.reply(buildPanel('Logs Retention', [
      `Retention ustawiony na **${retentionDays}** dni.`,
    ]));
    return;
  }

  if (mode === 'include_bots') {
    const value = interaction.options.getBoolean('value', false);
    if (typeof value !== 'boolean') {
      await interaction.reply(buildPanel('Logs Include Bots', [
        'Podaj `value:true` albo `value:false`.',
      ]));
      return;
    }

    stateStore.saveLogsState(guildId, {
      includeBots: value,
    });

    await interaction.reply(buildPanel('Logs Include Bots', [
      `Include bots: **${value ? 'yes' : 'no'}**`,
    ]));
    return;
  }

  if (mode === 'categories') {
    const category = String(interaction.options.getString('category', false) || '').trim().toLowerCase();
    const value = interaction.options.getBoolean('value', false);

    if (!category && typeof value !== 'boolean') {
      await interaction.reply(buildPanel('Logs Categories', buildCategoriesLines(current)));
      return;
    }

    if (!category || typeof value !== 'boolean') {
      await interaction.reply(buildPanel('Logs Categories', [
        'Aby zmienic kategorie, podaj oba pola: `category` i `value`.',
        '`value:true` wlacza kategorie, `value:false` ja wylacza.',
      ]));
      return;
    }

    if (category === 'all') {
      if (value !== true) {
        await interaction.reply(buildPanel('Logs Categories', [
          'Dla `category:all` uzyj tylko `value:true`, aby wlaczyc wszystkie kategorie.',
          'Jesli chcesz wylaczyc wszystko, uzyj `/logs mode:disable`.',
        ]));
        return;
      }

      stateStore.saveLogsState(guildId, {
        disabledCategories: [],
      });
      const next = resolveLogsState(stateStore, guildId);
      await interaction.reply(buildPanel('Logs Categories', [
        'Wlaczono wszystkie kategorie logow.',
        ...buildCategoriesLines(next),
      ]));
      return;
    }

    if (!LOG_CATEGORY_KEYS.includes(category)) {
      await interaction.reply(buildPanel('Logs Categories', [
        `Nieznana kategoria: \`${category}\`.`,
        `Dostepne: ${LOG_CATEGORY_KEYS.join(', ')}`,
      ]));
      return;
    }

    const disabled = new Set(current.disabledCategories);
    if (value) {
      disabled.delete(category);
    } else {
      disabled.add(category);
    }

    const nextState = {
      ...current,
      disabledCategories: [...disabled].sort(),
    };
    if (getEnabledCategories(nextState).length === 0) {
      await interaction.reply(buildPanel('Logs Categories', [
        'Nie mozna wylaczyc wszystkich kategorii naraz.',
        'Zostaw przynajmniej jedna aktywna albo uzyj `/logs mode:disable`.',
      ]));
      return;
    }

    stateStore.saveLogsState(guildId, {
      disabledCategories: nextState.disabledCategories,
    });

    const next = resolveLogsState(stateStore, guildId);
    await interaction.reply(buildPanel('Logs Categories', [
      `${value ? 'Wlaczono' : 'Wylaczono'} kategorie **${CATEGORY_META[category]?.label || category}**.`,
      ...buildCategoriesLines(next),
    ]));
    return;
  }

  if (mode === 'preview') {
    const payload = await fullLogs?.createPreviewPayload(guildId);
    await interaction.reply(payload || buildPanel('Logs Preview', [
      'Nie udalo sie przygotowac podgladu loga.',
    ]));
    return;
  }

  if (mode === 'export_day') {
    const day = String(interaction.options.getString('day', false) || '').trim();
    if (!isValidDayKey(day)) {
      await interaction.reply(buildPanel('Logs Export', [
        'Podaj `day` w formacie `YYYY-MM-DD`.',
      ]));
      return;
    }

    try {
      const exported = await fullLogs?.exportDay(guildId, day);
      if (!exported) {
        await interaction.reply(buildPanel('Logs Export', [
          `Brak logu dla dnia \`${day}\`.`,
        ]));
        return;
      }

      await interaction.reply({
        flags: EPHEMERAL_FLAG,
        content: `Eksport dnia \`${day}\` gotowy.`,
        files: [
          new AttachmentBuilder(exported.filePath, {
            name: exported.fileName,
          }),
        ],
      });
      return;
    } catch (error) {
      await interaction.reply(buildPanel('Logs Export', [
        `Nie udalo sie wyeksportowac logu: ${truncateInline(error.message, 220)}`,
      ]));
      return;
    }
  }

  if (mode === 'purge') {
    const day = String(interaction.options.getString('day', false) || '').trim();
    const olderThanDays = interaction.options.getInteger('older_than_days', false);
    const confirm = interaction.options.getBoolean('confirm', false) === true;

    if (!confirm) {
      await interaction.reply(buildPanel('Logs Purge', [
        'Purge wymaga jawnego potwierdzenia `confirm:true`.',
        'Przyklady:',
        '`/logs mode:purge day:2026-06-02 confirm:true`',
        '`/logs mode:purge older_than_days:30 confirm:true`',
      ]));
      return;
    }

    if (!day && !Number.isFinite(olderThanDays)) {
      await interaction.reply(buildPanel('Logs Purge', [
        'Podaj `day` albo `older_than_days`.',
      ]));
      return;
    }

    if (day && Number.isFinite(olderThanDays)) {
      await interaction.reply(buildPanel('Logs Purge', [
        'W jednym purge wybierz tylko jeden zakres: `day` albo `older_than_days`.',
      ]));
      return;
    }

    try {
      const result = await fullLogs?.purgeLogs(guildId, {
        day: day || null,
        olderThanDays: Number.isFinite(olderThanDays) ? olderThanDays : null,
      });

      const removedPreview = Array.isArray(result?.removedDays) && result.removedDays.length > 0
        ? result.removedDays.slice(0, 10).join(', ')
        : 'brak';

      await interaction.reply(buildPanel('Logs Purge', [
        result?.mode === 'day'
          ? (result?.removed > 0
            ? `Usunieto wpis dla dnia: \`${day}\`.`
            : `Nie znaleziono logu dla dnia: \`${day}\`.`)
          : `Usunieto logi starsze niz **${result?.olderThanDays || olderThanDays}** dni.`,
        `Liczba usunietych plikow: **${result?.removed || 0}**`,
        `Zakres usunietych dni: ${removedPreview}`,
        Array.isArray(result?.removedDays) && result.removedDays.length > 10
          ? `... i jeszcze ${result.removedDays.length - 10} kolejnych dni.`
          : null,
      ].filter(Boolean)));
      return;
    } catch (error) {
      await interaction.reply(buildPanel('Logs Purge', [
        `Purge nie powiodl sie: ${truncateInline(error.message, 220)}`,
      ]));
      return;
    }
  }

  if (mode === 'test') {
    const latest = resolveLogsState(stateStore, guildId);
    if (!latest.enabled || !latest.channelId) {
      await interaction.reply(buildPanel('Logs Test', [
        'Najpierw ustaw kanal i wlacz logs:',
        '`/logs mode:setup channel:#kanal`',
      ]));
      return;
    }

    await fullLogs?.sendTestLog(guildId, interaction.user);

    await interaction.reply(buildPanel('Logs Test', [
      `Wyslalem test wpis na <#${latest.channelId}>.`,
      'Sprawdz czy karta Components v2 jest czytelna.',
    ]));
    return;
  }

  await interaction.reply(buildPanel('Logs', [
    `Nieznany mode: \`${mode}\`.`,
  ]));
}

module.exports = {
  logsCommand,
  executeLogsCommand,
};
