const {
  ApplicationCommandOptionType,
  EmbedBuilder,
  PermissionFlagsBits,
} = require('discord.js');
const {
  formatConfigError,
  readOperatorConfigSync,
  writeOperatorConfigSync,
} = require('../core/config');

const SECTION_CHOICES = [
  { name: 'autoclean', value: 'autoclean' },
  { name: 'automod', value: 'automod' },
  { name: 'verify', value: 'verify' },
];

const settingsCommand = {
  name: 'settings',
  description: 'Pokazuje i zmienia ustawienia operatora',
  dm_permission: false,
  default_member_permissions: PermissionFlagsBits.Administrator.toString(),
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Tryb komendy settings',
      required: true,
      choices: [
        { name: 'show', value: 'show' },
        { name: 'set', value: 'set' },
      ],
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'section',
      description: 'Sekcja ustawien',
      required: false,
      choices: SECTION_CHOICES,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'option',
      description: 'Klucz opcji, np. dry_run lub enabled',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'string_value',
      description: 'Wartosc tekstowa albo "clear"',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Integer,
      name: 'integer_value',
      description: 'Wartosc liczbowa',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Boolean,
      name: 'boolean_value',
      description: 'Wartosc true/false',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Channel,
      name: 'channel_value',
      description: 'Wartosc kanalu lub kanal dla podgladu channel_settings',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Role,
      name: 'role_value',
      description: 'Wartosc roli',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.User,
      name: 'user_value',
      description: 'Wartosc uzytkownika',
      required: false,
    },
  ],
};

function normalizeOptionKey(value) {
  return String(value || '')
    .trim()
    .toLowerCase()
    .replace(/[\s.-]+/g, '_');
}

function formatValue(value) {
  if (value === null || value === undefined || value === '') return 'brak';
  if (typeof value === 'boolean') return value ? 'tak' : 'nie';
  if (Array.isArray(value)) return value.length === 0 ? '[]' : value.map((item) => String(item)).join(', ');
  if (typeof value === 'object') return `\`\`\`json\n${JSON.stringify(value, null, 2)}\n\`\`\``;
  return String(value);
}

function truncateValue(value, max = 1024) {
  const text = String(value ?? 'brak');
  if (text.length <= max) return text;
  return `${text.slice(0, max - 15)}...[obcieto]`;
}

function formatFieldValue(value) {
  return truncateValue(formatValue(value));
}

function createBaseEmbed(title, description = null) {
  const embed = new EmbedBuilder()
    .setColor(0x2f9e44)
    .setTitle(title)
    .setTimestamp();

  if (description) {
    embed.setDescription(description);
  }

  return embed;
}

function addSnapshotFields(embed, snapshot) {
  for (const [key, value] of Object.entries(snapshot)) {
    embed.addFields({
      name: key,
      value: formatFieldValue(value),
      inline: false,
    });
  }
  return embed;
}

function getPathValue(obj, path) {
  return path.reduce((current, key) => (current && Object.prototype.hasOwnProperty.call(current, key) ? current[key] : undefined), obj);
}

function setPathValue(obj, path, value) {
  let current = obj;
  for (let i = 0; i < path.length - 1; i += 1) {
    const key = path[i];
    if (!current[key] || typeof current[key] !== 'object' || Array.isArray(current[key])) {
      current[key] = {};
    }
    current = current[key];
  }
  current[path[path.length - 1]] = value;
}

function getVerifySnapshot(stateStore, guildId) {
  const verify = stateStore.getVerificationState(guildId);
  const panel = verify.panel || {};
  return {
    enabled: verify.enabled === true,
    role: verify.roleId ? `<@&${verify.roleId}> (${verify.roleId})` : 'brak',
    panel_channel: panel.channelId ? `<#${panel.channelId}> (${panel.channelId})` : 'brak',
    panel_message_id: panel.messageId || 'brak',
    panel_custom_id: panel.customId || 'brak',
    exceptions_count: Array.isArray(verify.exceptions) ? verify.exceptions.length : 0,
    exceptions: Array.isArray(verify.exceptions) && verify.exceptions.length > 0
      ? verify.exceptions.map((id) => `<@${id}> (${id})`).join(', ')
      : 'brak',
    active_expirations: verify.expirations ? Object.keys(verify.expirations).length : 0,
    active_challenges: verify.challenges ? Object.keys(verify.challenges).length : 0,
    active_lockouts: verify.lockouts ? Object.keys(verify.lockouts).length : 0,
    last_sweep: verify.lastSweepAt || 'nigdy',
  };
}

function buildDefinitions(stateStore, guildId) {
  return {
    autoclean: {
      label: 'AutoClean',
      options: {
        enabled: {
          type: 'boolean',
          path: ['enabled'],
          description: 'Wlacza lub wylacza caly AutoClean',
        },
        cron_schedule: {
          type: 'string',
          path: ['cronSchedule'],
          description: 'Cron wykonywania czyszczenia',
        },
        timezone: {
          type: 'string',
          path: ['timezone'],
          description: 'Timezone cron, np. Europe/Warsaw',
        },
        delete_messages_older_than_days: {
          type: 'integer',
          path: ['deleteMessagesOlderThanDays'],
          description: 'Usuwaj wiadomosci starsze niz X dni',
        },
        delete_limit: {
          type: 'integer',
          path: ['deleteLimit'],
          description: 'Limit usuwanych wiadomosci na kanal',
        },
        delete_pinned: {
          type: 'boolean',
          path: ['deletePinned'],
          description: 'Czy usuwac przypiete wiadomosci',
        },
        dry_run: {
          type: 'boolean',
          path: ['dryRun'],
          description: 'Tryb testowy bez usuwania',
        },
        log_channel: {
          type: 'channel',
          path: ['logChannelId'],
          description: 'Kanal logow AutoClean',
          formatter: (value) => (value ? `<#${value}>` : 'brak'),
        },
        notify_enabled: {
          type: 'boolean',
          path: ['notifyBeforeDelete', 'enabled'],
          description: 'Wlacza ostrzezenia przed czyszczeniem',
        },
        notify_minutes_before: {
          type: 'integer',
          path: ['notifyBeforeDelete', 'minutesBefore'],
          description: 'Ile minut przed czyszczeniem wyslac ostrzezenie',
        },
        notify_message: {
          type: 'string',
          path: ['notifyBeforeDelete', 'message'],
          description: 'Tresc ostrzezenia przed czyszczeniem',
        },
      },
      snapshot: (config) => ({
        enabled: config.enabled !== false,
        cron_schedule: config.cronSchedule || '0 3 * * 0',
        timezone: config.timezone || 'Europe/Warsaw',
        delete_messages_older_than_days: Number(config.deleteMessagesOlderThanDays || 0) || 0,
        delete_limit: Number(config.deleteLimit || 0) || 0,
        delete_pinned: config.deletePinned === true,
        dry_run: config.dryRun === true,
        log_channel: config.logChannelId ? `<#${config.logChannelId}>` : 'brak',
        notify_enabled: config.notifyBeforeDelete?.enabled === true,
        notify_minutes_before: Number(config.notifyBeforeDelete?.minutesBefore || 0) || 0,
        notify_message: config.notifyBeforeDelete?.message || null,
        channels_enabled: Array.isArray(config.channels) ? config.channels.filter((item) => item.enabled !== false).length : 0,
      }),
    },
    automod: {
      label: 'AutoMod',
      options: {
        enabled: {
          type: 'boolean',
          path: ['automod', 'enabled'],
          description: 'Wlacza lub wylacza AutoMod',
        },
        log_channel: {
          type: 'channel',
          path: ['automod', 'logChannelId'],
          description: 'Kanal logow AutoMod',
          formatter: (value) => (value ? `<#${value}>` : 'brak'),
        },
        block_invites: {
          type: 'boolean',
          path: ['automod', 'rules', 'blockInvites'],
          description: 'Blokowanie linkow zaproszen Discord',
        },
        block_links: {
          type: 'boolean',
          path: ['automod', 'rules', 'blockLinks'],
          description: 'Blokowanie linkow spoza allowlisty',
        },
        max_mentions: {
          type: 'integer',
          path: ['automod', 'rules', 'maxMentions'],
          description: 'Maksymalna liczba mentionow',
        },
        caps_enabled: {
          type: 'boolean',
          path: ['automod', 'rules', 'caps', 'enabled'],
          description: 'Wlacza filtr caps spam',
        },
        caps_min_length: {
          type: 'integer',
          path: ['automod', 'rules', 'caps', 'minLength'],
          description: 'Minimalna liczba liter dla caps spam',
        },
        caps_ratio: {
          type: 'string',
          path: ['automod', 'rules', 'caps', 'ratio'],
          description: 'Procent wielkich liter, np. 0.75',
        },
        duplicate_enabled: {
          type: 'boolean',
          path: ['automod', 'rules', 'duplicate', 'enabled'],
          description: 'Wlacza filtr duplikatow',
        },
        duplicate_window_seconds: {
          type: 'integer',
          path: ['automod', 'rules', 'duplicate', 'windowSeconds'],
          description: 'Okno czasowe dla duplikatow',
        },
        duplicate_max_repeats: {
          type: 'integer',
          path: ['automod', 'rules', 'duplicate', 'maxRepeats'],
          description: 'Maksymalna liczba powtorek',
        },
        delete_message: {
          type: 'boolean',
          path: ['automod', 'actions', 'delete'],
          description: 'Czy usuwac wiadomosc po trafieniu',
        },
        timeout_minutes: {
          type: 'integer',
          path: ['automod', 'actions', 'timeoutMinutes'],
          description: 'Timeout po wykryciu naruszenia',
        },
        warn_in_channel: {
          type: 'boolean',
          path: ['automod', 'actions', 'warnInChannel'],
          description: 'Czy wyslac ostrzezenie na kanale',
        },
        warn_template: {
          type: 'string',
          path: ['automod', 'actions', 'warnTemplate'],
          description: 'Template ostrzezenia',
        },
        warn_delete_after_seconds: {
          type: 'integer',
          path: ['automod', 'actions', 'warnDeleteAfterSeconds'],
          description: 'Po ilu sekundach usunac ostrzezenie',
        },
      },
      snapshot: (config) => ({
        enabled: config.automod?.enabled === true,
        log_channel: config.automod?.logChannelId ? `<#${config.automod.logChannelId}>` : 'brak',
        block_invites: config.automod?.rules?.blockInvites === true,
        block_links: config.automod?.rules?.blockLinks === true,
        max_mentions: Number(config.automod?.rules?.maxMentions || 0) || 0,
        caps_enabled: config.automod?.rules?.caps?.enabled === true,
        caps_min_length: Number(config.automod?.rules?.caps?.minLength || 0) || 0,
        caps_ratio: Number(config.automod?.rules?.caps?.ratio || 0) || 0,
        duplicate_enabled: config.automod?.rules?.duplicate?.enabled === true,
        duplicate_window_seconds: Number(config.automod?.rules?.duplicate?.windowSeconds || 0) || 0,
        duplicate_max_repeats: Number(config.automod?.rules?.duplicate?.maxRepeats || 0) || 0,
        delete_message: config.automod?.actions?.delete === true,
        timeout_minutes: Number(config.automod?.actions?.timeoutMinutes || 0) || 0,
        warn_in_channel: config.automod?.actions?.warnInChannel === true,
        warn_template: config.automod?.actions?.warnTemplate || null,
        warn_delete_after_seconds: Number(config.automod?.actions?.warnDeleteAfterSeconds || 0) || 0,
      }),
    },
    verify: {
      label: 'Verify',
      options: {
        enabled: {
          type: 'boolean',
          getter: () => stateStore.getVerificationState(guildId).enabled === true,
          setter: (value) => stateStore.saveVerificationState(guildId, { enabled: value === true }),
          description: 'Wlacza lub wylacza system verify',
        },
        role: {
          type: 'role',
          getter: () => stateStore.getVerificationState(guildId).roleId || null,
          setter: (value) => stateStore.saveVerificationState(guildId, { roleId: value || null }),
          description: 'Rola nadawana po verify',
          formatter: (value) => (value ? `<@&${value}>` : 'brak'),
        },
        exceptions: {
          type: 'info',
          getter: () => stateStore.listVerificationExceptions(guildId),
          description: 'Lista wyjatkow verify',
          formatter: (value) => (Array.isArray(value) && value.length > 0
            ? value.map((id) => `<@${id}> (${id})`).join(', ')
            : 'brak'),
        },
        exception_add: {
          type: 'user',
          getter: () => stateStore.listVerificationExceptions(guildId),
          setter: (value) => stateStore.addVerificationException(guildId, value),
          description: 'Dodaje usera do wyjatkow verify',
          formatter: (value) => (Array.isArray(value) && value.length > 0
            ? value.map((id) => `<@${id}> (${id})`).join(', ')
            : 'brak'),
        },
        exception_remove: {
          type: 'user',
          getter: () => stateStore.listVerificationExceptions(guildId),
          setter: (value) => stateStore.removeVerificationException(guildId, value),
          description: 'Usuwa usera z wyjatkow verify',
          formatter: (value) => (Array.isArray(value) && value.length > 0
            ? value.map((id) => `<@${id}> (${id})`).join(', ')
            : 'brak'),
        },
      },
      snapshot: () => getVerifySnapshot(stateStore, guildId),
    },
  };
}

function getSectionState(definition, config) {
  return typeof definition.snapshot === 'function' ? definition.snapshot(config) : {};
}

function getOptionCurrentValue(definition, config, key) {
  const option = definition.options[key];
  if (!option) return undefined;
  if (typeof option.getter === 'function') {
    return option.getter();
  }
  return getPathValue(config, option.path);
}

function formatOptionValue(definition, key, value) {
  const option = definition.options[key];
  if (!option) return formatValue(value);
  if (typeof option.formatter === 'function') {
    return option.formatter(value);
  }
  return formatValue(value);
}

function coerceSetValue(option, values) {
  if (option.type === 'boolean') {
    if (typeof values.booleanValue === 'boolean') return values.booleanValue;
    throw new Error('Ta opcja wymaga `boolean_value`.');
  }

  if (option.type === 'integer') {
    if (Number.isInteger(values.integerValue)) return values.integerValue;
    throw new Error('Ta opcja wymaga `integer_value`.');
  }

  if (option.type === 'string') {
    const raw = String(values.stringValue || '').trim();
    if (!raw) {
      throw new Error('Ta opcja wymaga `string_value`.');
    }
    if (option.path?.join('.') === 'automod.rules.caps.ratio') {
      const parsed = Number(raw);
      if (!Number.isFinite(parsed) || parsed < 0 || parsed > 1) {
        throw new Error('caps_ratio musi byc liczba od 0 do 1.');
      }
      return parsed;
    }
    return raw;
  }

  if (option.type === 'channel') {
    if (values.channelValue) return values.channelValue.id;
    if (String(values.stringValue || '').trim().toLowerCase() === 'clear') return '';
    throw new Error('Ta opcja wymaga `channel_value` albo `string_value: clear`.');
  }

  if (option.type === 'role') {
    if (values.roleValue) return values.roleValue.id;
    if (String(values.stringValue || '').trim().toLowerCase() === 'clear') return null;
    throw new Error('Ta opcja wymaga `role_value` albo `string_value: clear`.');
  }

  if (option.type === 'user') {
    if (values.userValue) return values.userValue.id;
    throw new Error('Ta opcja wymaga `user_value`.');
  }

  throw new Error('Nieobslugiwany typ opcji.');
}

async function executeSettingsCommand(interaction, { cleaner, automod, stateStore, configPath }) {
  if (!interaction.inGuild()) {
    await interaction.reply({ content: 'Ta komenda dziala tylko na serwerze.', ephemeral: true });
    return;
  }

  if (!interaction.memberPermissions?.has(PermissionFlagsBits.Administrator)) {
    await interaction.reply({ content: 'Brak uprawnien Administratora.', ephemeral: true });
    return;
  }

  const mode = String(interaction.options.getString('mode', true) || '').toLowerCase();
  const section = interaction.options.getString('section', false);
  const optionKey = normalizeOptionKey(interaction.options.getString('option', false));
  const showChannel = interaction.options.getChannel('channel_value', false);
  let config;
  try {
    config = readOperatorConfigSync(configPath);
  } catch (error) {
    await interaction.reply({
      content: formatConfigError(error, { prefix: 'Nie mozna odczytac config.json. ' }),
      ephemeral: true,
    });
    return;
  }
  const definitions = buildDefinitions(stateStore, interaction.guildId);

  if (mode === 'show') {
    if (!section) {
      const embed = createBaseEmbed(
        'Operator Settings',
        'Podglad najwazniejszych sekcji ustawien.',
      );
      for (const [sectionKey, definition] of Object.entries(definitions)) {
        const snapshot = getSectionState(definition, config);
        const enabledValue = Object.prototype.hasOwnProperty.call(snapshot, 'enabled')
          ? formatValue(snapshot.enabled)
          : 'n/a';
        embed.addFields({
          name: definition.label,
          value: `Status: ${enabledValue}\nUzyj \`/settings mode:show section:${sectionKey}\``,
          inline: false,
        });
      }
      embed.setFooter({ text: 'Sekcje: autoclean, automod, verify' });
      await interaction.reply({ embeds: [embed], ephemeral: true });
      return;
    }

    const definition = definitions[section];
    if (!definition) {
      await interaction.reply({ content: `Nieznana sekcja: ${section}`, ephemeral: true });
      return;
    }

    if (!optionKey) {
      const snapshot = getSectionState(definition, config);
      const embed = createBaseEmbed(
        `${definition.label} Settings`,
        `Dostepne opcje: ${Object.keys(definition.options).join(', ')}`,
      );
      addSnapshotFields(embed, snapshot);
      await interaction.reply({ embeds: [embed], ephemeral: true });
      return;
    }

    if (section === 'autoclean' && optionKey === 'channel_settings') {
      const channel = showChannel || interaction.channel;
      if (!channel || !channel.isTextBased()) {
        await interaction.reply({
          content: 'Podaj `channel_value` albo uzyj komendy na kanale tekstowym.',
          ephemeral: true,
        });
        return;
      }

      const settings = cleaner.getEffectiveChannelSettings(channel.id);
      const snapshot = {
        enabled: settings.enabled ? 'tak' : 'nie',
        delete_limit: settings.deleteLimit || 'brak',
        delete_pinned: settings.deletePinned ? 'tak' : 'nie',
        delete_messages_older_than_days: settings.deleteMessagesOlderThanDays || 0,
        whitelist_users: settings.whitelistUsers.length,
        whitelist_roles: settings.whitelistRoles.length,
        notify_before_delete: settings.notifyBeforeDeleteEnabled ? `${settings.notifyBeforeDeleteMinutes} min` : 'off',
      };
      const embed = createBaseEmbed(
        'AutoClean Channel Settings',
        `Kanal: <#${channel.id}>`,
      );
      addSnapshotFields(embed, snapshot);
      await interaction.reply({ embeds: [embed], ephemeral: true });
      return;
    }

    const option = definition.options[optionKey];
    if (!option) {
      await interaction.reply({
        content: `Nieznana opcja \`${optionKey}\`. Dostepne: ${Object.keys(definition.options).join(', ')}, channel_settings`,
        ephemeral: true,
      });
      return;
    }

    const currentValue = getOptionCurrentValue(definition, config, optionKey);
    const embed = createBaseEmbed(`${definition.label} / ${optionKey}`);
    embed.addFields(
      {
        name: 'Typ',
        value: truncateValue(option.type),
        inline: true,
      },
      {
        name: 'Aktualnie',
        value: truncateValue(formatOptionValue(definition, optionKey, currentValue)),
        inline: true,
      },
      {
        name: 'Opis',
        value: truncateValue(option.description || 'brak'),
        inline: false,
      },
    );
    await interaction.reply({ embeds: [embed], ephemeral: true });
    return;
  }

  if (mode === 'set') {
    if (!section) {
      await interaction.reply({ content: 'Dla mode=set podaj `section`.', ephemeral: true });
      return;
    }
    if (!optionKey) {
      await interaction.reply({ content: 'Dla mode=set podaj `option`.', ephemeral: true });
      return;
    }

    const definition = definitions[section];
    if (!definition) {
      await interaction.reply({ content: `Nieznana sekcja: ${section}`, ephemeral: true });
      return;
    }

    const option = definition.options[optionKey];
    if (!option) {
      await interaction.reply({
        content: `Nieznana opcja \`${optionKey}\`. Dostepne: ${Object.keys(definition.options).join(', ')}`,
        ephemeral: true,
      });
      return;
    }

    let nextValue;
    try {
      nextValue = coerceSetValue(option, {
        stringValue: interaction.options.getString('string_value', false),
        integerValue: interaction.options.getInteger('integer_value', false),
        booleanValue: interaction.options.getBoolean('boolean_value', false),
        channelValue: interaction.options.getChannel('channel_value', false),
        roleValue: interaction.options.getRole('role_value', false),
        userValue: interaction.options.getUser('user_value', false),
      });
    } catch (error) {
      await interaction.reply({ content: error.message, ephemeral: true });
      return;
    }

    if (typeof option.setter === 'function') {
      option.setter(nextValue);
    } else {
      setPathValue(config, option.path, nextValue);
      try {
        writeOperatorConfigSync(configPath, config);
      } catch (error) {
        await interaction.reply({
          content: formatConfigError(error, { prefix: 'Nie mozna zapisac config.json. ' }),
          ephemeral: true,
        });
        return;
      }
    }

    try {
      if (section === 'autoclean') {
        cleaner.start();
      }

      if (section === 'automod') {
        automod.start();
      }
    } catch (error) {
      await interaction.reply({
        content: formatConfigError(error, { prefix: 'Ustawienie zapisano, ale modul nie mogl przeladowac configu. ' }),
        ephemeral: true,
      });
      return;
    }

    let updatedConfig;
    try {
      updatedConfig = readOperatorConfigSync(configPath);
    } catch (error) {
      await interaction.reply({
        content: formatConfigError(error, { prefix: 'Ustawienie zapisano, ale nie udalo sie ponownie odczytac config.json. ' }),
        ephemeral: true,
      });
      return;
    }
    const currentValue = getOptionCurrentValue(definition, updatedConfig, optionKey);
    await interaction.reply({
      content: `Zmieniono \`${section}.${optionKey}\` na: ${formatOptionValue(definition, optionKey, currentValue)}`,
      ephemeral: true,
    });
    return;
  }

  await interaction.reply({
    content: `Nieznany mode settings: ${mode}.`,
    ephemeral: true,
  });
}

module.exports = {
  settingsCommand,
  executeSettingsCommand,
};
