const { ApplicationCommandOptionType, ChannelType, EmbedBuilder } = require('discord.js');
const { isOwner } = require('../core/owners');
const {
  deleteAnnouncementMessage,
  editAnnouncementMessage,
  ensureAnnouncementStateShape,
  findAnnouncementByReference,
  generateAnnouncementId,
  generateAnnouncementCode,
  getAnnouncementReference,
  normalizeAnnouncementProfileName,
  normalizeAnnouncementMode,
  normalizeAnnouncementUrl,
  normalizeColorValue,
  normalizeOptionalText,
  parseAnnouncementDateTime,
  sendAnnouncementNow,
} = require('../core/announcements');

const announceCommand = {
  name: 'announce',
  description: 'Planowanie ogloszen przez webhook',
  dm_permission: false,
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Tryb komendy announce',
      required: true,
      choices: [
        { name: 'create', value: 'create' },
        { name: 'now', value: 'now' },
        { name: 'list', value: 'list' },
        { name: 'edit', value: 'edit' },
        { name: 'delete', value: 'delete' },
        { name: 'profile_create', value: 'profile_create' },
        { name: 'profile_list', value: 'profile_list' },
        { name: 'profile_edit', value: 'profile_edit' },
        { name: 'profile_delete', value: 'profile_delete' },
      ],
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'datetime',
      description: 'Data i godzina UTC, np. 2026-04-20 18:30',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'content',
      description: 'Tresc ogloszenia',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'profile',
      description: 'Preset profilu (dla create/now)',
      required: false,
      autocomplete: true,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'name',
      description: 'Nazwa profilu (dla profile_*)',
      required: false,
      autocomplete: true,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'id',
      description: 'ID/kod ogloszenia (dla edit/delete)',
      required: false,
      autocomplete: true,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'send_mode',
      description: 'Jak wyslac announcement',
      required: false,
      choices: [
        { name: 'message', value: 'message' },
        { name: 'embed', value: 'embed' },
        { name: 'components', value: 'components' },
      ],
    },
    {
      type: ApplicationCommandOptionType.Channel,
      name: 'channel',
      description: 'Kanal docelowy',
      required: false,
      channel_types: [ChannelType.GuildText, ChannelType.GuildAnnouncement],
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'title',
      description: 'Tytul embeda',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'color',
      description: 'Kolor embeda, np. #2F9E44',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'username',
      description: 'Nazwa webhooka',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'avatar_url',
      description: 'Avatar webhooka (URL)',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'button_label',
      description: 'Label przycisku dla components',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'button_url',
      description: 'URL przycisku dla components',
      required: false,
    },
  ],
};

function buildListEmbed(items) {
  const embed = new EmbedBuilder()
    .setColor(0x2f9e44)
    .setTitle('Announcements')
    .setTimestamp();

  if (!items.length) {
    embed.setDescription('Brak ogloszen.');
    return embed;
  }

  for (const item of items.slice(0, 25)) {
    embed.addFields({
      name: `${getAnnouncementReference(item)} • ${item.status}`,
      value: [
        `kanal: <#${item.channelId}>`,
        `czas: <t:${Math.floor(new Date(item.runAt).getTime() / 1000)}:F>`,
        item.status !== 'sent' && item.code ? `kod: \`${item.code}\`` : null,
        item.messageId ? `messageId: \`${item.messageId}\`` : null,
        Array.isArray(item.messageIds) && item.messageIds.length > 1 ? `parts: ${item.messageIds.length}` : null,
        `tryb: ${item.sendMode || (item.title || item.color ? 'embed' : 'message')}`,
        item.profileName ? `profil: \`${item.profileName}\`` : null,
        item.title ? `tytul: ${item.title}` : null,
        item.buttonLabel ? `button: ${item.buttonLabel}` : null,
        item.lastError ? `blad: ${item.lastError}` : null,
      ].filter(Boolean).join('\n'),
      inline: false,
    });
  }

  return embed;
}

function buildProfileListEmbed(profiles) {
  const entries = Object.entries(profiles || {}).sort(([a], [b]) => a.localeCompare(b, 'pl'));
  const embed = new EmbedBuilder()
    .setColor(0x1864ab)
    .setTitle('Announcement Profiles')
    .setTimestamp();

  if (!entries.length) {
    embed.setDescription('Brak profili.');
    return embed;
  }

  for (const [name, profile] of entries.slice(0, 25)) {
    embed.addFields({
      name,
      value: [
        `kanal: <#${profile.channelId}>`,
        profile.title ? `tytul: ${profile.title}` : null,
        profile.color ? `kolor: ${profile.color}` : null,
        profile.username ? `username: ${profile.username}` : null,
        profile.avatarURL ? `avatar: ${profile.avatarURL}` : null,
      ].filter(Boolean).join('\n'),
      inline: false,
    });
  }

  return embed;
}

function resolveAnnouncementProfile(profiles, rawName) {
  const profileName = normalizeAnnouncementProfileName(rawName);
  if (!profileName) return { profileName: null, profile: null };
  return {
    profileName,
    profile: profiles?.[profileName] || null,
  };
}

function ensureAnnouncementCodes(items) {
  const used = new Set();
  let mutated = false;

  for (const item of items || []) {
    let nextCode = String(item?.code || item?.id || '').trim().toUpperCase();
    const valid = /^[A-Z0-9]{4}$/.test(nextCode);
    if (!valid || used.has(nextCode)) {
      do {
        nextCode = generateAnnouncementCode();
      } while (used.has(nextCode));
      item.code = nextCode;
      mutated = true;
    } else if (item.code !== nextCode) {
      item.code = nextCode;
      mutated = true;
    }
    used.add(nextCode);
  }

  return mutated;
}

function normalizeAnnouncementContent(value, { required = false } = {}) {
  const applyLineBreakFormatting = (input) => {
    let text = String(input || '')
      .replace(/\r\n/g, '\n')
      .replace(/\\n/g, '\n')
      .replace(/\\t/g, '  ')
      .replace(/<br\s*\/?>/gi, '\n');

    if (!text.includes('\n')) {
      text = text
        .replace(/\s+(#{1,6}\s)/g, '\n\n$1')
        .replace(/\s+(>{1,3}\s)/g, '\n$1')
        .replace(/\s+(-#\s)/g, '\n$1')
        .replace(/\s+-\s+/g, '\n- ')
        .replace(/\s+\*\s+/g, '\n* ')
        .replace(/\s+(\d+\.\s)/g, '\n$1');
    }

    const lines = text.split('\n');
    let inCodeBlock = false;
    const normalizedLines = lines.map((line) => {
      const trimmed = line.trim();
      if (trimmed.startsWith('```')) {
        inCodeBlock = !inCodeBlock;
        return line;
      }
      if (inCodeBlock || trimmed.length === 0) {
        return line;
      }

      return line
        .replace(/^(\s*)(#{1,6})(\S)/, '$1$2 $3')
        .replace(/^(\s*)-#(\S)/, '$1-# $2')
        .replace(/^(\s*)(>{1,3})(\S)/, '$1$2 $3')
        .replace(/^(\s*)([-*])(\S)/, '$1$2 $3')
        .replace(/^(\s*)(\d+\.)(\S)/, '$1$2 $3');
    });

    return normalizedLines.join('\n')
      .replace(/\n{3,}/g, '\n\n')
      .trim();
  };

  if (required) {
    const raw = String(value || '').trim();
    if (!raw) {
      throw new Error('Tresc announcement nie moze byc pusta.');
    }
    return applyLineBreakFormatting(raw);
  }

  const normalized = normalizeOptionalText(value);
  if (normalized === undefined || normalized === null) return normalized;
  return applyLineBreakFormatting(normalized);
}

function generateUnusedAnnouncementCode(items) {
  const used = new Set(
    (items || [])
      .map((entry) => String(entry?.code || '').trim().toUpperCase())
      .filter((code) => /^[A-Z0-9]{4}$/.test(code)),
  );

  let nextCode = generateAnnouncementCode();
  while (used.has(nextCode)) {
    nextCode = generateAnnouncementCode();
  }
  return nextCode;
}

function applyAnnouncementDelivery(item, sent, { timestamp = new Date().toISOString(), preserveSentAt = false } = {}) {
  item.status = 'sent';
  if (!preserveSentAt || !item.sentAt) {
    item.sentAt = timestamp;
  }
  item.updatedAt = timestamp;
  item.webhookId = sent.webhookId || item.webhookId || null;
  item.deliveryChannelId = sent.deliveryChannelId || item.channelId || item.deliveryChannelId || null;
  item.messageId = sent.messageId || null;
  item.messageIds = Array.isArray(sent.messageIds) && sent.messageIds.length > 0
    ? sent.messageIds
    : (sent.messageId ? [sent.messageId] : []);
  item.lastError = null;
}

async function executeAnnounceAutocomplete(interaction, { stateStore }) {
  if (!interaction.inGuild() || !isOwner(interaction.user.id)) {
    await interaction.respond([]);
    return;
  }

  const current = ensureAnnouncementStateShape(stateStore.getAnnouncementsState(interaction.guildId));
  const focused = interaction.options.getFocused(true);
  const query = String(focused.value || '').trim().toLowerCase();

  if (focused.name === 'profile' || focused.name === 'name') {
    const choices = Object.keys(current.profiles || {})
      .sort((a, b) => a.localeCompare(b, 'pl'))
      .filter((name) => !query || name.toLowerCase().includes(query))
      .slice(0, 25)
      .map((name) => ({ name, value: name }));
    await interaction.respond(choices);
    return;
  }

  if (focused.name === 'id') {
    ensureAnnouncementCodes(current.items);
    const choices = current.items
      .slice()
      .sort((a, b) => new Date(a.runAt).getTime() - new Date(b.runAt).getTime())
      .map((item) => {
        const ref = getAnnouncementReference(item);
        const label = [
          ref,
          item.status,
          item.profileName || null,
          item.title || (item.content ? item.content.slice(0, 32) : null),
        ].filter(Boolean).join(' • ');
        return { label: label.slice(0, 100), value: ref };
      })
      .filter((item) => !query || item.label.toLowerCase().includes(query) || String(item.value).toLowerCase().includes(query))
      .slice(0, 25)
      .map((item) => ({ name: item.label, value: item.value }));
    await interaction.respond(choices);
    return;
  }

  await interaction.respond([]);
}

function requireOwner(interaction) {
  if (isOwner(interaction.user.id)) return true;
  interaction.reply({
    content: 'Ta komenda jest tylko dla ownera.',
    ephemeral: true,
  }).catch(() => {});
  return false;
}

async function executeAnnounceCommand(interaction, { stateStore, logger }) {
  if (!interaction.inGuild()) {
    await interaction.reply({ content: 'Ta komenda dziala tylko na serwerze.', ephemeral: true });
    return;
  }

  if (!requireOwner(interaction)) return;

  const guildId = interaction.guildId;
  const mode = String(interaction.options.getString('mode', true) || '').toLowerCase();
  const current = ensureAnnouncementStateShape(stateStore.getAnnouncementsState(guildId));
  const codeMutated = ensureAnnouncementCodes(current.items);
  if (codeMutated) {
    stateStore.saveAnnouncementsState(guildId, { items: current.items });
  }

  if (mode === 'profile_create') {
    const rawName = interaction.options.getString('name', false);
    if (!rawName) {
      await interaction.reply({ content: 'Dla mode=profile_create podaj `name`.', ephemeral: true });
      return;
    }
    const name = normalizeAnnouncementProfileName(rawName);
    const channel = interaction.options.getChannel('channel', false);
    if (!channel) {
      await interaction.reply({ content: 'Dla mode=profile_create podaj `channel`.', ephemeral: true });
      return;
    }

    const title = normalizeOptionalText(interaction.options.getString('title', false));
    const color = normalizeColorValue(interaction.options.getString('color', false));
    const username = normalizeOptionalText(interaction.options.getString('username', false));
    const avatarURL = normalizeOptionalText(interaction.options.getString('avatar_url', false));

    if (current.profiles[name]) {
      await interaction.reply({ content: 'Profil o tej nazwie juz istnieje.', ephemeral: true });
      return;
    }

    current.profiles[name] = {
      channelId: channel.id,
      title: title || null,
      color: color || null,
      username: username || null,
      avatarURL: avatarURL || null,
      createdBy: interaction.user.id,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };

    stateStore.saveAnnouncementsState(guildId, { profiles: current.profiles });
    await interaction.reply({
      content: `Dodano profil \`${name}\` dla <#${channel.id}>.`,
      ephemeral: true,
    });
    return;
  }

  if (mode === 'profile_list') {
    await interaction.reply({
      embeds: [buildProfileListEmbed(current.profiles)],
      ephemeral: true,
    });
    return;
  }

  if (mode === 'profile_edit') {
    const rawName = interaction.options.getString('name', false);
    if (!rawName) {
      await interaction.reply({ content: 'Dla mode=profile_edit podaj `name`.', ephemeral: true });
      return;
    }
    const name = normalizeAnnouncementProfileName(rawName);
    const profile = current.profiles[name];
    if (!profile) {
      await interaction.reply({ content: 'Nie znaleziono profilu o tej nazwie.', ephemeral: true });
      return;
    }

    const nextChannel = interaction.options.getChannel('channel', false);
    const nextTitle = normalizeOptionalText(interaction.options.getString('title', false));
    const nextColor = normalizeColorValue(interaction.options.getString('color', false));
    const nextUsername = normalizeOptionalText(interaction.options.getString('username', false));
    const nextAvatar = normalizeOptionalText(interaction.options.getString('avatar_url', false));
    const changed = [];

    if (nextChannel) {
      profile.channelId = nextChannel.id;
      changed.push('channel');
    }
    if (nextTitle !== undefined) {
      profile.title = nextTitle || null;
      changed.push('title');
    }
    if (nextColor !== undefined) {
      profile.color = nextColor || null;
      changed.push('color');
    }
    if (nextUsername !== undefined) {
      profile.username = nextUsername || null;
      changed.push('username');
    }
    if (nextAvatar !== undefined) {
      profile.avatarURL = nextAvatar || null;
      changed.push('avatar_url');
    }

    if (!changed.length) {
      await interaction.reply({ content: 'Nie podano zadnej zmiany.', ephemeral: true });
      return;
    }

    profile.updatedAt = new Date().toISOString();
    stateStore.saveAnnouncementsState(guildId, { profiles: current.profiles });
    await interaction.reply({
      content: `Zmieniono profil \`${name}\`: ${changed.join(', ')}.`,
      ephemeral: true,
    });
    return;
  }

  if (mode === 'profile_delete') {
    const rawName = interaction.options.getString('name', false);
    if (!rawName) {
      await interaction.reply({ content: 'Dla mode=profile_delete podaj `name`.', ephemeral: true });
      return;
    }
    const name = normalizeAnnouncementProfileName(rawName);
    if (!current.profiles[name]) {
      await interaction.reply({ content: 'Nie znaleziono profilu o tej nazwie.', ephemeral: true });
      return;
    }

    delete current.profiles[name];
    stateStore.saveAnnouncementsState(guildId, { profiles: current.profiles });
    await interaction.reply({
      content: `Usunieto profil \`${name}\`.`,
      ephemeral: true,
    });
    return;
  }

  if (mode === 'create') {
    const { profileName, profile } = resolveAnnouncementProfile(
      current.profiles,
      interaction.options.getString('profile', false),
    );
    if (interaction.options.getString('profile', false) && !profile) {
      await interaction.reply({ content: 'Nie znaleziono profilu o tej nazwie.', ephemeral: true });
      return;
    }

    const profileChannel = profile
      ? await interaction.guild.channels.fetch(profile.channelId).catch(() => null)
      : null;
    const channel = interaction.options.getChannel('channel', false) || profileChannel;
    if (!channel) {
      await interaction.reply({
        content: 'Podaj `channel` albo uzyj profilu z ustawionym kanalem.',
        ephemeral: true,
      });
      return;
    }
    const datetimeInput = interaction.options.getString('datetime', false);
    if (!datetimeInput) {
      await interaction.reply({ content: 'Dla mode=create podaj `datetime`.', ephemeral: true });
      return;
    }
    const runAt = parseAnnouncementDateTime(datetimeInput);
    const contentInput = interaction.options.getString('content', false);
    if (!contentInput) {
      await interaction.reply({ content: 'Dla mode=create podaj `content`.', ephemeral: true });
      return;
    }
    const content = normalizeAnnouncementContent(
      contentInput,
      { required: true },
    );
    const sendModeOption = normalizeAnnouncementMode(interaction.options.getString('send_mode', false));
    const title = normalizeOptionalText(interaction.options.getString('title', false));
    const color = normalizeColorValue(interaction.options.getString('color', false));
    const username = normalizeOptionalText(interaction.options.getString('username', false));
    const avatarURL = normalizeOptionalText(interaction.options.getString('avatar_url', false));
    const buttonLabel = normalizeOptionalText(interaction.options.getString('button_label', false));
    const buttonURL = normalizeAnnouncementUrl(interaction.options.getString('button_url', false));

    const sendMode = sendModeOption || ((buttonLabel || buttonURL)
      ? 'components'
      : ((title || color) ? 'embed' : 'message'));
    if (sendMode === 'components' && (!buttonLabel || !buttonURL)) {
      await interaction.reply({
        content: 'Tryb `components` wymaga `button_label` i `button_url`.',
        ephemeral: true,
      });
      return;
    }

    const item = {
      id: generateAnnouncementId(),
      code: generateUnusedAnnouncementCode(current.items),
      channelId: channel.id,
      runAt,
      content,
      sendMode,
      profileName,
      title: title !== undefined ? (title || null) : (profile?.title || null),
      color: color !== undefined ? (color || null) : (profile?.color || null),
      username: username !== undefined ? (username || null) : (profile?.username || null),
      avatarURL: avatarURL !== undefined ? (avatarURL || null) : (profile?.avatarURL || null),
      buttonLabel: buttonLabel !== undefined ? (buttonLabel || null) : null,
      buttonURL: buttonURL !== undefined ? (buttonURL || null) : null,
      status: 'scheduled',
      createdBy: interaction.user.id,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      sentAt: null,
      messageId: null,
      messageIds: [],
      webhookId: null,
      deliveryChannelId: null,
      lastError: null,
    };

    current.items.push(item);
    stateStore.saveAnnouncementsState(guildId, { items: current.items });
    await interaction.reply({
      content: `Dodano ogloszenie \`${item.code}\` na <t:${Math.floor(new Date(runAt).getTime() / 1000)}:F> (UTC) w <#${channel.id}>${profileName ? ` z profilu \`${profileName}\`` : ''}.`,
      ephemeral: true,
    });
    return;
  }

  if (mode === 'now') {
    const { profileName, profile } = resolveAnnouncementProfile(
      current.profiles,
      interaction.options.getString('profile', false),
    );
    if (interaction.options.getString('profile', false) && !profile) {
      await interaction.reply({ content: 'Nie znaleziono profilu o tej nazwie.', ephemeral: true });
      return;
    }

    const profileChannel = profile
      ? await interaction.guild.channels.fetch(profile.channelId).catch(() => null)
      : null;
    const channel = interaction.options.getChannel('channel', false) || profileChannel;
    if (!channel) {
      await interaction.reply({
        content: 'Podaj `channel` albo uzyj profilu z ustawionym kanalem.',
        ephemeral: true,
      });
      return;
    }

    const contentInput = interaction.options.getString('content', false);
    if (!contentInput) {
      await interaction.reply({ content: 'Dla mode=now podaj `content`.', ephemeral: true });
      return;
    }
    const content = normalizeAnnouncementContent(
      contentInput,
      { required: true },
    );
    const sendModeOption = normalizeAnnouncementMode(interaction.options.getString('send_mode', false));
    const title = normalizeOptionalText(interaction.options.getString('title', false));
    const color = normalizeColorValue(interaction.options.getString('color', false));
    const username = normalizeOptionalText(interaction.options.getString('username', false));
    const avatarURL = normalizeOptionalText(interaction.options.getString('avatar_url', false));
    const buttonLabel = normalizeOptionalText(interaction.options.getString('button_label', false));
    const buttonURL = normalizeAnnouncementUrl(interaction.options.getString('button_url', false));

    const sendMode = sendModeOption || ((buttonLabel || buttonURL)
      ? 'components'
      : ((title || color) ? 'embed' : 'message'));
    if (sendMode === 'components' && (!buttonLabel || !buttonURL)) {
      await interaction.reply({
        content: 'Tryb `components` wymaga `button_label` i `button_url`.',
        ephemeral: true,
      });
      return;
    }

    const nowIso = new Date().toISOString();
    const item = {
      id: generateAnnouncementId(),
      code: generateUnusedAnnouncementCode(current.items),
      channelId: channel.id,
      runAt: nowIso,
      content,
      sendMode,
      profileName,
      title: title !== undefined ? (title || null) : (profile?.title || null),
      color: color !== undefined ? (color || null) : (profile?.color || null),
      username: username !== undefined ? (username || null) : (profile?.username || null),
      avatarURL: avatarURL !== undefined ? (avatarURL || null) : (profile?.avatarURL || null),
      buttonLabel: buttonLabel !== undefined ? (buttonLabel || null) : null,
      buttonURL: buttonURL !== undefined ? (buttonURL || null) : null,
      status: 'scheduled',
      createdBy: interaction.user.id,
      createdAt: nowIso,
      updatedAt: nowIso,
      sentAt: null,
      messageId: null,
      messageIds: [],
      webhookId: null,
      deliveryChannelId: null,
      lastError: null,
    };

    await interaction.deferReply({ ephemeral: true });

    try {
      const sent = await sendAnnouncementNow({
        client: interaction.client,
        stateStore,
        guild: interaction.guild,
        item,
        logger,
      });

      applyAnnouncementDelivery(item, sent);

      current.items.push(item);
      stateStore.saveAnnouncementsState(guildId, { items: current.items });
      const sentParts = Array.isArray(item.messageIds) ? item.messageIds.length : 1;
      await interaction.editReply(
        `Wyslano ogloszenie \`${item.code}\` teraz w <#${channel.id}> (parts: ${sentParts}, messageId: \`${item.messageId}\`)${profileName ? ` z profilu \`${profileName}\`` : ''}.`,
      );
    } catch (error) {
      item.status = 'failed';
      item.lastError = error.message;
      item.updatedAt = new Date().toISOString();
      current.items.push(item);
      stateStore.saveAnnouncementsState(guildId, { items: current.items });
      logger?.error?.(`Announcement now error (${guildId}/${item.id}): ${error.message}`);
      await interaction.editReply(`Nie udalo sie wyslac announcement teraz: ${error.message}`);
    }
    return;
  }

  if (mode === 'list') {
    current.items.sort((a, b) => new Date(a.runAt).getTime() - new Date(b.runAt).getTime());
    await interaction.reply({
      embeds: [buildListEmbed(current.items)],
      ephemeral: true,
    });
    return;
  }

  if (mode === 'edit') {
    const id = String(interaction.options.getString('id', false) || '').trim();
    if (!id) {
      await interaction.reply({ content: 'Dla mode=edit podaj `id`.', ephemeral: true });
      return;
    }
    const item = findAnnouncementByReference(current.items, id);
    if (!item) {
      await interaction.reply({ content: 'Nie znaleziono ogloszenia o tym ID.', ephemeral: true });
      return;
    }

    const nextChannel = interaction.options.getChannel('channel', false);
    const nextDatetimeRaw = interaction.options.getString('datetime', false);
    const nextContent = normalizeAnnouncementContent(interaction.options.getString('content', false));
    const nextTitle = normalizeOptionalText(interaction.options.getString('title', false));
    const nextColor = normalizeColorValue(interaction.options.getString('color', false));
    const nextUsername = normalizeOptionalText(interaction.options.getString('username', false));
    const nextAvatar = normalizeOptionalText(interaction.options.getString('avatar_url', false));
    const nextMode = normalizeAnnouncementMode(interaction.options.getString('send_mode', false));
    const nextButtonLabel = normalizeOptionalText(interaction.options.getString('button_label', false));
    const nextButtonURL = normalizeAnnouncementUrl(interaction.options.getString('button_url', false));
    const previousItem = {
      ...item,
      messageIds: Array.isArray(item.messageIds) ? [...item.messageIds] : [],
    };

    const changed = [];
    if (nextChannel) {
      item.channelId = nextChannel.id;
      changed.push('channel');
    }
    if (nextDatetimeRaw) {
      item.runAt = parseAnnouncementDateTime(nextDatetimeRaw);
      changed.push('datetime');
    }
    if (nextContent !== undefined) {
      item.content = nextContent || '';
      changed.push('content');
    }
    if (nextTitle !== undefined) {
      item.title = nextTitle || null;
      changed.push('title');
    }
    if (nextColor !== undefined) {
      item.color = nextColor || null;
      changed.push('color');
    }
    if (nextUsername !== undefined) {
      item.username = nextUsername || null;
      changed.push('username');
    }
    if (nextAvatar !== undefined) {
      item.avatarURL = nextAvatar || null;
      changed.push('avatar_url');
    }
    if (nextMode !== undefined) {
      item.sendMode = nextMode || 'message';
      changed.push('mode');
    }
    if (nextButtonLabel !== undefined) {
      item.buttonLabel = nextButtonLabel || null;
      changed.push('button_label');
    }
    if (nextButtonURL !== undefined) {
      item.buttonURL = nextButtonURL || null;
      changed.push('button_url');
    }

    if (changed.length === 0) {
      await interaction.reply({ content: 'Nie podano zadnej zmiany.', ephemeral: true });
      return;
    }

    if (item.sendMode === 'components' && (!item.buttonLabel || !item.buttonURL)) {
      await interaction.reply({
        content: 'Tryb `components` wymaga `button_label` i `button_url`.',
        ephemeral: true,
      });
      return;
    }

    item.updatedAt = new Date().toISOString();
    item.lastError = null;
    let liveUpdateNote = null;
    let liveUpdateError = null;

    if (item.status === 'sent' && (item.messageId || (Array.isArray(item.messageIds) && item.messageIds.length > 0))) {
      try {
        const edited = await editAnnouncementMessage({
          client: interaction.client,
          stateStore,
          guild: interaction.guild,
          item,
          previousItem,
          logger,
        });
        if (edited?.messageId) {
          item.messageId = edited.messageId;
          item.messageIds = Array.isArray(edited.messageIds) ? edited.messageIds : [edited.messageId];
          item.webhookId = edited.webhookId || item.webhookId || null;
          item.deliveryChannelId = edited.deliveryChannelId || item.deliveryChannelId || item.channelId || null;
        }
        if (edited?.mode === 'resent') {
          liveUpdateNote = 'live update: ponowna publikacja na nowy kanal';
        } else if (edited?.mode === 'edited_in_place') {
          liveUpdateNote = 'live update: edycja w miejscu';
        }
      } catch (error) {
        item.lastError = error.message;
        liveUpdateError = error;
        logger?.error?.(`Announcement edit live error (${guildId}/${item.id}): ${error.message}`);
      }
    } else {
      item.status = 'scheduled';
    }

    stateStore.saveAnnouncementsState(guildId, { items: current.items });
    const baseReply = `Zmieniono ogloszenie \`${getAnnouncementReference(item)}\`: ${changed.join(', ')}.`;
    if (liveUpdateError) {
      await interaction.reply({
        content: `${baseReply} Zapisano zmiany w stanie, ale aktualizacja juz wyslanej wiadomosci nie powiodla sie: ${liveUpdateError.message}`,
        ephemeral: true,
      });
      return;
    }
    await interaction.reply({
      content: liveUpdateNote ? `${baseReply} ${liveUpdateNote}.` : baseReply,
      ephemeral: true,
    });
    return;
  }

  if (mode === 'delete') {
    const id = String(interaction.options.getString('id', false) || '').trim();
    if (!id) {
      await interaction.reply({ content: 'Dla mode=delete podaj `id`.', ephemeral: true });
      return;
    }
    const index = current.items.findIndex((entry) => findAnnouncementByReference([entry], id));
    if (index === -1) {
      await interaction.reply({ content: 'Nie znaleziono ogloszenia o tym ID.', ephemeral: true });
      return;
    }

    const [item] = current.items.splice(index, 1);
    let deleteWarning = null;
    if (item.status === 'sent' && (item.messageId || (Array.isArray(item.messageIds) && item.messageIds.length > 0))) {
      await deleteAnnouncementMessage({
        client: interaction.client,
        stateStore,
        guildId,
        item,
        logger,
      }).catch((error) => {
        deleteWarning = error.message;
        logger?.warn?.(`Announcement delete live warn (${guildId}/${item.id}): ${error.message}`);
      });
    }

    stateStore.saveAnnouncementsState(guildId, { items: current.items });
    await interaction.reply({
      content: deleteWarning
        ? `Usunieto ogloszenie \`${getAnnouncementReference(item)}\` ze stanu, ale nie udalo sie skasowac opublikowanej wiadomosci: ${deleteWarning}`
        : `Usunieto ogloszenie \`${getAnnouncementReference(item)}\`.`,
      ephemeral: true,
    });
    return;
  }

  await interaction.reply({
    content: `Nieznany mode announce: ${mode}.`,
    ephemeral: true,
  });
}

module.exports = {
  announceCommand,
  executeAnnounceAutocomplete,
  executeAnnounceCommand,
};
