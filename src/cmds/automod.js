const { ApplicationCommandOptionType, PermissionFlagsBits } = require('discord.js');
const {
  formatConfigError,
  readOperatorConfigSync,
  writeOperatorConfigSync,
} = require('../core/config');

const RULE_TYPE_META = {
  word: {
    label: 'Blocked words',
  },
  regex: {
    label: 'Blocked regex',
  },
  allowed_domain: {
    label: 'Allowed domains',
  },
};

const RULE_TYPE_CHOICES = Object.entries(RULE_TYPE_META).map(([value, meta]) => ({
  name: meta.label,
  value,
}));

const RULE_LIST_CHOICES = [
  { name: 'All managed rules', value: 'all' },
  ...RULE_TYPE_CHOICES,
];

const TEST_TYPE_CHOICES = [
  { name: 'Active rules', value: 'live' },
  { name: 'Blocked words', value: 'word' },
  { name: 'Blocked regex', value: 'regex' },
];

const automodCommand = {
  name: 'automod',
  description: 'Status i zarzadzanie AutoMod',
  dm_permission: false,
  default_member_permissions: PermissionFlagsBits.Administrator.toString(),
  options: [
    {
      type: ApplicationCommandOptionType.Subcommand,
      name: 'status',
      description: 'Pokazuje status AutoMod',
    },
    {
      type: ApplicationCommandOptionType.Subcommand,
      name: 'test',
      description: 'Sprawdza czy tresc wpada w regule AutoMod',
      options: [
        {
          type: ApplicationCommandOptionType.String,
          name: 'content',
          description: 'Tresc do sprawdzenia',
          required: true,
        },
        {
          type: ApplicationCommandOptionType.String,
          name: 'type',
          description: 'Test aktywnych regul albo konkretnej kandydatki',
          required: false,
          choices: TEST_TYPE_CHOICES,
        },
        {
          type: ApplicationCommandOptionType.String,
          name: 'value',
          description: 'Wartosc konkretnej reguly dla word/regex',
          required: false,
        },
      ],
    },
    {
      type: ApplicationCommandOptionType.SubcommandGroup,
      name: 'rule',
      description: 'Zarzadzanie listami regul AutoMod',
      options: [
        {
          type: ApplicationCommandOptionType.Subcommand,
          name: 'list',
          description: 'Listuje zapisane reguly',
          options: [
            {
              type: ApplicationCommandOptionType.String,
              name: 'type',
              description: 'Pokazuje wybrana grupe regul albo wszystko',
              required: false,
              choices: RULE_LIST_CHOICES,
            },
          ],
        },
        {
          type: ApplicationCommandOptionType.Subcommand,
          name: 'add',
          description: 'Dodaje nowa regule do configu',
          options: [
            {
              type: ApplicationCommandOptionType.String,
              name: 'type',
              description: 'Typ reguly',
              required: true,
              choices: RULE_TYPE_CHOICES,
            },
            {
              type: ApplicationCommandOptionType.String,
              name: 'value',
              description: 'Wartosc reguly',
              required: true,
            },
          ],
        },
        {
          type: ApplicationCommandOptionType.Subcommand,
          name: 'remove',
          description: 'Usuwa regule z configu',
          options: [
            {
              type: ApplicationCommandOptionType.String,
              name: 'type',
              description: 'Typ reguly',
              required: true,
              choices: RULE_TYPE_CHOICES,
            },
            {
              type: ApplicationCommandOptionType.String,
              name: 'value',
              description: 'Wartosc reguly',
              required: true,
            },
          ],
        },
      ],
    },
  ],
};

function replyEphemeral(interaction, content) {
  return interaction.reply({
    content,
    ephemeral: true,
  });
}

function normalizeWordRule(value) {
  const normalized = String(value || '').replace(/\s+/g, ' ').trim().toLowerCase();
  if (!normalized) {
    throw new Error('Regula slowa nie moze byc pusta.');
  }
  return normalized;
}

function normalizeRegexRule(value) {
  const normalized = String(value || '').trim();
  if (!normalized) {
    throw new Error('Regula regex nie moze byc pusta.');
  }
  try {
    new RegExp(normalized);
  } catch (error) {
    throw new Error(`Niepoprawny regex: ${error.message}`);
  }
  return normalized;
}

function normalizeDomainRule(value) {
  const raw = String(value || '').trim().toLowerCase();
  if (!raw) {
    throw new Error('Dozwolona domena nie moze byc pusta.');
  }

  let hostname;
  try {
    const candidate = raw.includes('://') ? raw : `https://${raw}`;
    hostname = new URL(candidate).hostname.toLowerCase();
  } catch {
    throw new Error('Niepoprawna domena. Uzyj np. `example.com`.');
  }

  const normalized = hostname.replace(/\.+$/g, '');
  if (!normalized) {
    throw new Error('Niepoprawna domena. Uzyj np. `example.com`.');
  }
  return normalized;
}

function normalizeRuleValue(type, value) {
  if (type === 'word') return normalizeWordRule(value);
  if (type === 'regex') return normalizeRegexRule(value);
  if (type === 'allowed_domain') return normalizeDomainRule(value);
  throw new Error(`Nieznany typ reguly: ${type}`);
}

function sanitizeManagedRuleList(type, values) {
  const input = Array.isArray(values) ? values : [];
  const seen = new Set();
  const normalized = [];

  for (const value of input) {
    try {
      const next = normalizeRuleValue(type, value);
      const key = type === 'regex' ? next : next.toLowerCase();
      if (seen.has(key)) continue;
      seen.add(key);
      normalized.push(next);
    } catch {
      // Skip malformed legacy values instead of crashing list/update flow.
    }
  }

  return normalized;
}

function ensureAutomodRules(config) {
  if (!config.automod || typeof config.automod !== 'object' || Array.isArray(config.automod)) {
    config.automod = {};
  }
  if (!config.automod.rules || typeof config.automod.rules !== 'object' || Array.isArray(config.automod.rules)) {
    config.automod.rules = {};
  }
  return config.automod.rules;
}

function getManagedRulesFromConfig(config) {
  const rules = ensureAutomodRules(config);
  return {
    word: sanitizeManagedRuleList('word', rules.blockedWords),
    regex: sanitizeManagedRuleList('regex', rules.blockedRegex),
    allowed_domain: sanitizeManagedRuleList('allowed_domain', rules.allowedDomains),
  };
}

function applyManagedRulesToConfig(config, managedRules) {
  const rules = ensureAutomodRules(config);
  rules.blockedWords = [...(managedRules.word || [])];
  rules.blockedRegex = [...(managedRules.regex || [])];
  rules.allowedDomains = [...(managedRules.allowed_domain || [])];
  return config;
}

function formatRuleValue(value) {
  return `\`${String(value || '')}\``;
}

function formatRulePreview(values, { empty = 'brak', limit = 10 } = {}) {
  if (!Array.isArray(values) || values.length === 0) {
    return empty;
  }
  const preview = values.slice(0, limit).map((value) => `- ${value}`).join('\n');
  if (values.length <= limit) {
    return preview;
  }
  return `${preview}\n- ... i jeszcze ${values.length - limit}`;
}

function buildRuleListLines(managedRules, filterType = 'all') {
  const types = filterType === 'all' ? Object.keys(RULE_TYPE_META) : [filterType];
  const lines = [];

  for (const type of types) {
    const meta = RULE_TYPE_META[type];
    const values = managedRules[type] || [];
    lines.push(`${meta.label} (${values.length}):`);
    lines.push(formatRulePreview(values));
    lines.push('');
  }

  return lines.filter((line, index, array) => !(line === '' && array[index - 1] === ''));
}

function readCurrentConfig(automod) {
  return readOperatorConfigSync(automod.configPath);
}

function writeAndReloadConfig(automod, nextConfig) {
  writeOperatorConfigSync(automod.configPath, nextConfig);
  automod.start();
}

function mutateManagedRule(automod, type, rawValue, action) {
  const normalizedValue = normalizeRuleValue(type, rawValue);
  const config = readCurrentConfig(automod);
  const managedRules = getManagedRulesFromConfig(config);
  const currentValues = managedRules[type] || [];
  const compareKey = type === 'regex' ? normalizedValue : normalizedValue.toLowerCase();
  const existingIndex = currentValues.findIndex((entry) => (
    (type === 'regex' ? entry : entry.toLowerCase()) === compareKey
  ));

  if (action === 'add') {
    if (existingIndex !== -1) {
      return {
        changed: false,
        reason: 'exists',
        normalizedValue,
        count: currentValues.length,
      };
    }

    managedRules[type] = [...currentValues, normalizedValue];
    applyManagedRulesToConfig(config, managedRules);
    writeAndReloadConfig(automod, config);
    return {
      changed: true,
      normalizedValue,
      count: managedRules[type].length,
    };
  }

  if (action === 'remove') {
    if (existingIndex === -1) {
      return {
        changed: false,
        reason: 'missing',
        normalizedValue,
        count: currentValues.length,
      };
    }

    managedRules[type] = currentValues.filter((_, index) => index !== existingIndex);
    applyManagedRulesToConfig(config, managedRules);
    writeAndReloadConfig(automod, config);
    return {
      changed: true,
      normalizedValue,
      count: managedRules[type].length,
    };
  }

  throw new Error(`Nieznana akcja reguly: ${action}`);
}

function buildStatusLines(automod) {
  const status = automod.getStatusSnapshot();
  const managedRules = automod.getManagedRules();

  return [
    `Enabled: ${status.enabled ? 'tak' : 'nie'}`,
    `Blocked words: ${status.blockedWords}`,
    `Blocked regex: ${status.blockedRegex}`,
    `Allowed domains: ${status.allowedDomains}`,
    `Block invites: ${status.blockInvites ? 'tak' : 'nie'}`,
    `Block links: ${status.blockLinks ? 'tak' : 'nie'}`,
    `Max mentions: ${status.maxMentions || 'off'}`,
    `Caps rule: ${status.capsEnabled ? `on (min=${status.capsMinLength}, ratio=${status.capsRatio})` : 'off'}`,
    `Duplicate rule: ${status.duplicateEnabled ? `on (${status.duplicateMaxRepeats}x / ${status.duplicateWindowSeconds}s)` : 'off'}`,
    `Delete message: ${status.deleteMessage ? 'tak' : 'nie'}`,
    `Timeout minutes: ${status.timeoutMinutes || 0}`,
    `Warn in channel: ${status.warnInChannel ? 'tak' : 'nie'}`,
    `Exempt users: ${status.exemptUsers}`,
    `Exempt roles: ${status.exemptRoles}`,
    `Log channel: ${status.logChannelId ? `<#${status.logChannelId}>` : 'brak'}`,
    '',
    'Managed rule preview:',
    `Words: ${formatRulePreview(managedRules.blockedWords, { empty: 'brak', limit: 3 }).replace(/\n/g, '; ')}`,
    `Regex: ${formatRulePreview(managedRules.blockedRegex, { empty: 'brak', limit: 3 }).replace(/\n/g, '; ')}`,
    `Domains: ${formatRulePreview(managedRules.allowedDomains, { empty: 'brak', limit: 3 }).replace(/\n/g, '; ')}`,
  ];
}

async function executeAutomodCommand(interaction, { automod }) {
  if (!interaction.inGuild()) {
    await replyEphemeral(interaction, 'Ta komenda dziala tylko na serwerze.');
    return;
  }

  if (!interaction.memberPermissions?.has(PermissionFlagsBits.Administrator)) {
    await replyEphemeral(interaction, 'Brak uprawnien Administratora.');
    return;
  }

  const subcommandGroup = interaction.options.getSubcommandGroup(false);
  const subcommand = interaction.options.getSubcommand();

  if (!subcommandGroup && subcommand === 'status') {
    await replyEphemeral(interaction, buildStatusLines(automod).join('\n'));
    return;
  }

  if (subcommandGroup === 'rule' && subcommand === 'list') {
    const type = String(interaction.options.getString('type', false) || 'all').trim().toLowerCase();
    if (type !== 'all' && !RULE_TYPE_META[type]) {
      await replyEphemeral(interaction, `Nieznany typ regul: \`${type}\`.`);
      return;
    }

    try {
      const config = readCurrentConfig(automod);
      const managedRules = getManagedRulesFromConfig(config);
      const header = type === 'all'
        ? 'AutoMod rules (all managed lists):'
        : `AutoMod rules: ${RULE_TYPE_META[type].label}`;
      await replyEphemeral(interaction, [header, '', ...buildRuleListLines(managedRules, type)].join('\n'));
      return;
    } catch (error) {
      await replyEphemeral(interaction, formatConfigError(error, { prefix: 'Nie mozna odczytac config.json. ' }));
      return;
    }
  }

  if (subcommandGroup === 'rule' && (subcommand === 'add' || subcommand === 'remove')) {
    const type = String(interaction.options.getString('type', true) || '').trim().toLowerCase();
    const rawValue = interaction.options.getString('value', true);
    if (!RULE_TYPE_META[type]) {
      await replyEphemeral(interaction, `Nieznany typ regul: \`${type}\`.`);
      return;
    }

    try {
      const result = mutateManagedRule(automod, type, rawValue, subcommand);
      const meta = RULE_TYPE_META[type];
      if (!result.changed) {
        const message = result.reason === 'exists'
          ? `${meta.label}: ${formatRuleValue(result.normalizedValue)} juz istnieje. Nic nie zmieniono.`
          : `${meta.label}: ${formatRuleValue(result.normalizedValue)} nie istnieje. Nic nie usunieto.`;
        await replyEphemeral(interaction, [
          message,
          `Aktualna liczba wpisow: ${result.count}`,
        ].join('\n'));
        return;
      }

      const verb = subcommand === 'add' ? 'Dodano' : 'Usunieto';
      await replyEphemeral(interaction, [
        `${verb} regule ${meta.label}: ${formatRuleValue(result.normalizedValue)}`,
        `Aktualna liczba wpisow: ${result.count}`,
      ].join('\n'));
      return;
    } catch (error) {
      await replyEphemeral(interaction, formatConfigError(error, { prefix: 'Zmiana reguly nie powiodla sie. ' }));
      return;
    }
  }

  if (!subcommandGroup && subcommand === 'test') {
    const content = interaction.options.getString('content', true);
    const type = String(interaction.options.getString('type', false) || 'live').trim().toLowerCase();
    const rawValue = interaction.options.getString('value', false);

    if (!TEST_TYPE_CHOICES.some((choice) => choice.value === type)) {
      await replyEphemeral(interaction, `Nieznany typ testu: \`${type}\`.`);
      return;
    }

    let normalizedCandidateValue = null;
    if (type !== 'live') {
      if (!rawValue) {
        await replyEphemeral(interaction, 'Dla testu konkretnej reguly podaj tez `value`.');
        return;
      }

      try {
        normalizedCandidateValue = normalizeRuleValue(type, rawValue);
      } catch (error) {
        await replyEphemeral(interaction, error.message);
        return;
      }
    }

    const result = automod.testContent(content, {
      mode: type,
      value: normalizedCandidateValue,
    });

    const lines = [];
    if (type === 'live') {
      lines.push('AutoMod test: aktywne reguly');
      lines.push(`Content: ${formatRuleValue(content)}`);
      if (result.matched) {
        lines.push(`Matched: ${result.violation.rule}`);
        lines.push(`Details: ${result.violation.details}`);
      } else {
        lines.push('Result: brak dopasowania w aktywnych regulach.');
      }
    } else {
      const meta = RULE_TYPE_META[type];
      lines.push(`AutoMod test: ${meta.label}`);
      lines.push(`Candidate: ${formatRuleValue(normalizedCandidateValue)}`);
      lines.push(`Content: ${formatRuleValue(content)}`);
      if (result.matched) {
        lines.push(`Result: trafia w te regule (${result.violation.rule}).`);
        lines.push(`Details: ${result.violation.details}`);
      } else {
        lines.push('Result: ta tresc nie trafia w te regule.');
      }
    }

    if (result.note) {
      lines.push(`Note: ${result.note}`);
    }

    await replyEphemeral(interaction, lines.join('\n'));
    return;
  }

  await replyEphemeral(interaction, 'Nieznana komenda automod.');
}

module.exports = {
  automodCommand,
  executeAutomodCommand,
};
