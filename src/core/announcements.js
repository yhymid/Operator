const {
  ActionRowBuilder,
  ButtonBuilder,
  ButtonStyle,
  EmbedBuilder,
} = require('discord.js');

const DEFAULT_WEBHOOK_NAME = 'Operator Announcements';
const ANNOUNCEMENT_CODE_ALPHABET = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
const DISCORD_MESSAGE_CONTENT_LIMIT = 2000;
const DISCORD_EMBED_DESCRIPTION_LIMIT = 4096;
const SPLIT_HINTS = ['\n\n', '\n', '. ', '! ', '? ', '; ', ', ', ' '];
let announcementSweepInFlight = null;

function generateAnnouncementId() {
  return `ann_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`;
}

function generateAnnouncementCode() {
  let code = '';
  for (let index = 0; index < 4; index += 1) {
    code += ANNOUNCEMENT_CODE_ALPHABET[Math.floor(Math.random() * ANNOUNCEMENT_CODE_ALPHABET.length)];
  }
  return code;
}

function parseAnnouncementDateTime(input) {
  const raw = String(input || '').trim();
  const match = raw.match(/^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})$/);
  if (!match) {
    throw new Error('Uzyj formatu daty `YYYY-MM-DD HH:mm` (UTC).');
  }

  const [, yearText, monthText, dayText, hourText, minuteText] = match;
  const year = Number(yearText);
  const month = Number(monthText);
  const day = Number(dayText);
  const hour = Number(hourText);
  const minute = Number(minuteText);
  const date = new Date(Date.UTC(year, month - 1, day, hour, minute, 0, 0));

  if (
    date.getUTCFullYear() !== year
    || date.getUTCMonth() !== month - 1
    || date.getUTCDate() !== day
    || date.getUTCHours() !== hour
    || date.getUTCMinutes() !== minute
  ) {
    throw new Error('Niepoprawna data lub godzina.');
  }

  return date.toISOString();
}

function normalizeOptionalText(value) {
  if (value === null || value === undefined) return undefined;
  const raw = String(value).trim();
  if (!raw) return undefined;
  if (raw === '-' || /^clear$/i.test(raw)) return null;
  return raw;
}

function normalizeColorValue(value) {
  const normalized = normalizeOptionalText(value);
  if (normalized === undefined || normalized === null) return normalized;
  const raw = normalized.replace(/^#/, '');
  if (!/^[0-9a-fA-F]{6}$/.test(raw)) {
    throw new Error('Kolor musi byc w formacie `#RRGGBB` albo `RRGGBB`.');
  }
  return `#${raw.toUpperCase()}`;
}

function normalizeAnnouncementProfileName(value) {
  const normalized = normalizeOptionalText(value);
  if (normalized === undefined || normalized === null) return normalized;
  const raw = normalized
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^[-_]+|[-_]+$/g, '');

  if (!/^[a-z0-9_-]{2,32}$/.test(raw)) {
    throw new Error('Nazwa profilu po normalizacji musi miec 2-32 znaki: litery, cyfry, `-` albo `_`.');
  }
  return raw;
}

function normalizeAnnouncementMode(value) {
  const normalized = normalizeOptionalText(value);
  if (normalized === undefined || normalized === null) return normalized;
  const raw = normalized.toLowerCase();
  if (!['message', 'embed', 'components'].includes(raw)) {
    throw new Error('Tryb wysylki musi byc jednym z: `message`, `embed`, `components`.');
  }
  return raw;
}

function normalizeAnnouncementUrl(value) {
  const normalized = normalizeOptionalText(value);
  if (normalized === undefined || normalized === null) return normalized;

  let url;
  try {
    url = new URL(normalized);
  } catch (error) {
    throw new Error('URL przycisku musi byc poprawnym adresem http albo https.');
  }

  if (!['http:', 'https:'].includes(url.protocol)) {
    throw new Error('URL przycisku musi zaczynac sie od `http://` albo `https://`.');
  }

  return url.toString();
}

function normalizeStoredMessageIds(item) {
  const ids = Array.isArray(item?.messageIds)
    ? item.messageIds
    : [];

  const normalized = ids
    .map((value) => String(value || '').trim())
    .filter(Boolean);

  if (item?.messageId) {
    normalized.unshift(String(item.messageId).trim());
  }

  return [...new Set(normalized)];
}

function ensureAnnouncementStateShape(state) {
  const items = Array.isArray(state?.items) ? [...state.items] : [];
  for (const item of items) {
    item.messageIds = normalizeStoredMessageIds(item);
    if (!item.messageId && item.messageIds.length > 0) {
      [item.messageId] = item.messageIds;
    }
    if (!item.deliveryChannelId && item.messageIds.length > 0) {
      item.deliveryChannelId = item.channelId || null;
    }
  }

  return {
    webhooks: state?.webhooks && typeof state.webhooks === 'object' ? { ...state.webhooks } : {},
    profiles: state?.profiles && typeof state.profiles === 'object' ? { ...state.profiles } : {},
    items,
    lastSweepAt: state?.lastSweepAt || null,
  };
}

function pickChunkSplitIndex(text, maxLength) {
  const max = Math.max(1, Number(maxLength) || 1);
  if (text.length <= max) return text.length;

  const min = Math.floor(max * 0.55);
  const window = text.slice(0, max);

  for (const marker of SPLIT_HINTS) {
    const index = window.lastIndexOf(marker);
    if (index >= min) {
      return marker.trim() ? index + marker.length : index + 1;
    }
  }

  return max;
}

function splitTextForDiscord(rawText, maxLength) {
  const text = String(rawText || '').trim();
  if (!text) return [''];

  const max = Math.max(1, Number(maxLength) || 1);
  if (text.length <= max) return [text];

  const chunks = [];
  let rest = text;
  while (rest.length > max) {
    const splitAt = pickChunkSplitIndex(rest, max);
    const head = rest.slice(0, splitAt).trimEnd();
    chunks.push(head || rest.slice(0, max));
    rest = rest.slice(splitAt).trimStart();
  }
  if (rest) chunks.push(rest);

  return chunks;
}

function buildAnnouncementPayloadParts(item) {
  const content = String(item.content || '').trim();
  const title = String(item.title || '').trim();
  const color = typeof item.color === 'string' ? item.color.trim() : '';
  const sendMode = item.sendMode || (title || color ? 'embed' : 'message');
  const embedNeeded = sendMode === 'embed' || (sendMode === 'components' && Boolean(title || color));
  const contentLimit = embedNeeded
    ? DISCORD_EMBED_DESCRIPTION_LIMIT
    : DISCORD_MESSAGE_CONTENT_LIMIT;
  const contentParts = splitTextForDiscord(content, contentLimit);

  if (sendMode === 'components') {
    const buttonLabel = String(item.buttonLabel || '').trim();
    const buttonURL = String(item.buttonURL || '').trim();
    if (!buttonLabel || !buttonURL) {
      throw new Error('Tryb `components` wymaga `button_label` i `button_url`.');
    }
  }

  return contentParts.map((chunk) => {
    const payload = {
      content: embedNeeded ? '' : chunk,
      username: String(item.username || DEFAULT_WEBHOOK_NAME),
    };

    if (item.avatarURL) {
      payload.avatarURL = item.avatarURL;
    }

    if (embedNeeded) {
      const embed = new EmbedBuilder();
      if (title) embed.setTitle(title);
      if (chunk) embed.setDescription(chunk);
      if (color) {
        embed.setColor(Number.parseInt(color.replace('#', ''), 16));
      }
      payload.embeds = [embed];
    }

    if (sendMode === 'components') {
      const buttonLabel = String(item.buttonLabel || '').trim();
      const buttonURL = String(item.buttonURL || '').trim();
      payload.content = embedNeeded ? (payload.content || '') : chunk;
      payload.components = [
        new ActionRowBuilder().addComponents(
          new ButtonBuilder()
            .setLabel(buttonLabel)
            .setStyle(ButtonStyle.Link)
            .setURL(buttonURL),
        ),
      ];
    }

    return payload;
  });
}

function buildAnnouncementPayload(item) {
  const [firstPayload] = buildAnnouncementPayloadParts(item);
  return firstPayload;
}

function getAnnouncementReference(item) {
  const messageIds = normalizeStoredMessageIds(item);
  if (item?.status === 'sent' && messageIds.length > 0) {
    return String(messageIds[0]);
  }
  if (item?.code) {
    return String(item.code);
  }
  if (item?.id) {
    return String(item.id);
  }
  return 'unknown';
}

function findAnnouncementByReference(items, rawReference) {
  const normalized = String(rawReference || '').trim().toUpperCase();
  return (items || []).find((item) => {
    const code = String(item?.code || item?.id || '').trim().toUpperCase();
    const messageIds = normalizeStoredMessageIds(item)
      .map((value) => value.toUpperCase());
    return normalized === code || messageIds.includes(normalized);
  }) || null;
}

function cloneWebhookRecord(record, channelId = null) {
  if (!record || typeof record !== 'object') return null;
  if (!record.id || !record.token) return null;
  return {
    id: String(record.id),
    token: String(record.token),
    channelId: channelId ? String(channelId) : null,
  };
}

function findStoredWebhookRecord(webhooks, { channelId = null, webhookId = null } = {}) {
  const normalizedChannelId = channelId ? String(channelId) : null;
  const normalizedWebhookId = webhookId ? String(webhookId) : null;
  const entries = Object.entries(webhooks || {});

  if (normalizedChannelId) {
    const direct = cloneWebhookRecord(webhooks?.[normalizedChannelId], normalizedChannelId);
    if (direct && (!normalizedWebhookId || direct.id === normalizedWebhookId)) {
      return direct;
    }
  }

  if (normalizedWebhookId) {
    for (const [entryChannelId, record] of entries) {
      const cloned = cloneWebhookRecord(record, entryChannelId);
      if (cloned?.id === normalizedWebhookId) {
        return cloned;
      }
    }
  }

  return null;
}

async function fetchWebhookWithToken({ client, webhookId, token, logger, context }) {
  if (!webhookId || !token) return null;

  try {
    return await client.fetchWebhook(webhookId, token);
  } catch (error) {
    logger?.warn?.(`Announcement webhook fetch failed (${context}): ${error.message}`);
    return null;
  }
}

async function resolveAnnouncementWebhook({
  client,
  stateStore,
  guildId,
  channelId = null,
  webhookId = null,
  legacyToken = null,
  logger,
}) {
  const current = ensureAnnouncementStateShape(stateStore.getAnnouncementsState(guildId));
  const stored = findStoredWebhookRecord(current.webhooks, { channelId, webhookId });
  const context = [guildId, channelId || 'unknown-channel', webhookId || 'unknown-webhook'].join('/');

  if (stored) {
    const webhook = await fetchWebhookWithToken({
      client,
      webhookId: stored.id,
      token: stored.token,
      logger,
      context,
    });
    if (webhook) {
      return {
        webhook,
        state: current,
        source: 'state',
        channelId: stored.channelId || channelId || null,
      };
    }
  }

  if (webhookId && legacyToken) {
    const webhook = await fetchWebhookWithToken({
      client,
      webhookId,
      token: legacyToken,
      logger,
      context: `${context}/legacy`,
    });
    if (webhook) {
      const resolvedChannelId = channelId || webhook.channelId || null;
      if (resolvedChannelId) {
        current.webhooks[String(resolvedChannelId)] = {
          id: webhook.id,
          token: String(legacyToken),
        };
        stateStore.saveAnnouncementsState(guildId, {
          webhooks: current.webhooks,
        });
      }
      return {
        webhook,
        state: current,
        source: 'legacy-item',
        channelId: resolvedChannelId,
      };
    }
  }

  return {
    webhook: null,
    state: current,
    source: 'missing',
    channelId: channelId || null,
  };
}

async function getOrCreateAnnouncementWebhook({ client, stateStore, guildId, channel, logger }) {
  const resolved = await resolveAnnouncementWebhook({
    client,
    stateStore,
    guildId,
    channelId: channel.id,
    logger,
  });

  if (resolved.webhook) {
    return {
      webhook: resolved.webhook,
      state: resolved.state,
    };
  }

  const created = await channel.createWebhook({
    name: DEFAULT_WEBHOOK_NAME,
    reason: 'Operator scheduled announcements',
  });

  const current = resolved.state;
  current.webhooks[String(channel.id)] = {
    id: created.id,
    token: created.token,
  };
  stateStore.saveAnnouncementsState(guildId, {
    webhooks: current.webhooks,
  });

  return {
    webhook: created,
    state: current,
  };
}

async function sendAnnouncementParts(webhook, item) {
  const payloads = buildAnnouncementPayloadParts(item);
  const sentMessages = [];

  for (const payload of payloads) {
    const sent = await webhook.send({
      ...payload,
      wait: true,
    });
    sentMessages.push(sent);
  }

  return sentMessages;
}

async function deleteAnnouncementParts(webhook, messageIds) {
  for (const messageId of messageIds) {
    await webhook.deleteMessage(messageId);
  }
}

async function sendAnnouncementNow({ client, stateStore, guild, item, logger }) {
  const channel = await guild.channels.fetch(item.channelId).catch(() => null);
  if (!channel || !channel.isTextBased()) {
    throw new Error('Docelowy kanal nie istnieje albo nie obsluguje wiadomosci.');
  }

  const { webhook } = await getOrCreateAnnouncementWebhook({
    client,
    stateStore,
    guildId: guild.id,
    channel,
    logger,
  });

  const sentMessages = await sendAnnouncementParts(webhook, item);
  const messageIds = sentMessages.map((message) => String(message.id));
  const [messageId] = messageIds;

  return {
    webhookId: webhook.id,
    deliveryChannelId: String(channel.id),
    messageId,
    messageIds,
  };
}

async function editAnnouncementMessage({
  client,
  stateStore,
  guild,
  item,
  previousItem = null,
  logger,
}) {
  const baseline = previousItem || item;
  const previousIds = normalizeStoredMessageIds(baseline);
  if (!baseline.webhookId || previousIds.length === 0) {
    throw new Error('Brak danych webhooka lub messageId do edycji.');
  }

  const deliveryChannelId = baseline.deliveryChannelId || baseline.channelId || null;
  const resolved = await resolveAnnouncementWebhook({
    client,
    stateStore,
    guildId: guild.id,
    channelId: deliveryChannelId,
    webhookId: baseline.webhookId,
    legacyToken: baseline.webhookToken || null,
    logger,
  });

  if (!resolved.webhook) {
    throw new Error('Nie udalo sie odzyskac webhooka dla juz wyslanego announcementu.');
  }

  const currentChannelId = String(item.channelId || '');
  const previousChannelId = String(deliveryChannelId || '');
  if (currentChannelId === previousChannelId) {
    const webhook = resolved.webhook;
    const payloads = buildAnnouncementPayloadParts(item);
    const messageIds = [];
    const sharedCount = Math.min(previousIds.length, payloads.length);

    for (let index = 0; index < sharedCount; index += 1) {
      const edited = await webhook.editMessage(previousIds[index], payloads[index]);
      messageIds.push(String(edited.id || previousIds[index]));
    }

    for (let index = sharedCount; index < payloads.length; index += 1) {
      const sent = await webhook.send({
        ...payloads[index],
        wait: true,
      });
      messageIds.push(String(sent.id));
    }

    if (payloads.length < previousIds.length) {
      await deleteAnnouncementParts(webhook, previousIds.slice(payloads.length));
    }

    const [messageId] = messageIds;
    return {
      mode: 'edited_in_place',
      webhookId: webhook.id,
      deliveryChannelId: previousChannelId || null,
      messageId,
      messageIds,
    };
  }

  const sent = await sendAnnouncementNow({
    client,
    stateStore,
    guild,
    item,
    logger,
  });

  try {
    await deleteAnnouncementParts(resolved.webhook, previousIds);
  } catch (error) {
    const rollbackItem = {
      messageId: sent.messageId,
      messageIds: sent.messageIds,
      webhookId: sent.webhookId,
      deliveryChannelId: sent.deliveryChannelId,
    };
    await deleteAnnouncementMessage({
      client,
      stateStore,
      guildId: guild.id,
      item: rollbackItem,
      logger,
    }).catch(() => {});
    throw new Error(`Przelaczono kanal, ale nie udalo sie usunac poprzedniej publikacji: ${error.message}`);
  }

  return {
    mode: 'resent',
    ...sent,
  };
}

async function deleteAnnouncementMessage({ client, stateStore, guildId, item, logger }) {
  const messageIds = normalizeStoredMessageIds(item);
  if (!item.webhookId || messageIds.length === 0) return;

  const deliveryChannelId = item.deliveryChannelId || item.channelId || null;
  const resolved = await resolveAnnouncementWebhook({
    client,
    stateStore,
    guildId,
    channelId: deliveryChannelId,
    webhookId: item.webhookId,
    legacyToken: item.webhookToken || null,
    logger,
  });

  if (!resolved.webhook) {
    throw new Error('Nie udalo sie odzyskac webhooka do usuniecia wyslanego announcementu.');
  }

  await deleteAnnouncementParts(resolved.webhook, messageIds);
}

async function performAnnouncementSweep({ client, stateStore, logger }) {
  const now = Date.now();
  const sweepTime = new Date(now).toISOString();
  const stats = {
    joined: false,
    guilds: 0,
    changedGuilds: 0,
    sent: 0,
    failed: 0,
    lastSweepAt: sweepTime,
  };

  for (const guild of client.guilds.cache.values()) {
    stats.guilds += 1;
    const announcementState = ensureAnnouncementStateShape(stateStore.getAnnouncementsState(guild.id));
    let mutated = false;

    for (const item of announcementState.items) {
      if (!item || item.status !== 'scheduled') continue;
      const runAtTs = Date.parse(item.runAt || '');
      if (!Number.isFinite(runAtTs) || runAtTs > now) continue;

      try {
        const sent = await sendAnnouncementNow({
          client,
          stateStore,
          guild,
          item,
          logger,
        });
        item.status = 'sent';
        item.sentAt = new Date().toISOString();
        item.webhookId = sent.webhookId;
        item.deliveryChannelId = sent.deliveryChannelId || item.channelId || null;
        item.messageId = sent.messageId;
        item.messageIds = sent.messageIds || [sent.messageId];
        item.lastError = null;
        mutated = true;
        stats.sent += 1;
      } catch (error) {
        item.status = 'failed';
        item.lastError = error.message;
        mutated = true;
        stats.failed += 1;
        logger?.error?.(`Announcement send error (${guild.id}/${item.id}): ${error.message}`);
      }
    }

    if (mutated) {
      stateStore.saveAnnouncementsState(guild.id, {
        items: announcementState.items,
        lastSweepAt: sweepTime,
      });
      stats.changedGuilds += 1;
    }
  }

  return stats;
}

async function runAnnouncementSweep({ client, stateStore, logger }) {
  if (announcementSweepInFlight) {
    const joined = await announcementSweepInFlight;
    return {
      ...joined,
      joined: true,
    };
  }

  announcementSweepInFlight = performAnnouncementSweep({ client, stateStore, logger });
  try {
    return await announcementSweepInFlight;
  } finally {
    announcementSweepInFlight = null;
  }
}

module.exports = {
  DEFAULT_WEBHOOK_NAME,
  buildAnnouncementPayload,
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
  runAnnouncementSweep,
  sendAnnouncementNow,
};
