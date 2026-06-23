const fs = require('fs');
const path = require('path');
const {
  AuditLogEvent,
  PermissionFlagsBits,
  Routes,
} = require('discord.js');

const fsp = fs.promises;
const COMPONENTS_V2_FLAG = 32768;
const AUDIT_CACHE_TTL_MS = 1500;
const AUDIT_MATCH_WINDOW_MS = 30 * 1000;
const RETENTION_SWEEP_INTERVAL_MS = 6 * 60 * 60 * 1000;
const WRITE_BATCH_DELAY_MS = 25;
const AUDIT_FETCH_LIMIT = 10;
const MIN_RETENTION_DAYS = 1;
const MAX_RETENTION_DAYS = 365;
const MAX_SUMMARY_LENGTH = 260;
const MAX_DETAIL_VALUE_LENGTH = 320;
const MAX_TEXT_DISPLAY_LENGTH = 1700;
const MAX_DETAIL_LINES = 12;
const MAX_RENDERED_DETAILS = 6;
const SNOWFLAKE_RE = /\b\d{15,22}\b/g;

const CATEGORY_META = {
  system: { label: 'System', icon: '🧩' },
  command: { label: 'Command', icon: '⚙️' },
  moderation: { label: 'Moderation', icon: '🛡️' },
  member: { label: 'Member', icon: '👤' },
  message: { label: 'Message', icon: '💬' },
  reaction: { label: 'Reaction', icon: '✨' },
  channel: { label: 'Channel', icon: '🧱' },
  role: { label: 'Role', icon: '🎭' },
  thread: { label: 'Thread', icon: '🧵' },
  voice: { label: 'Voice', icon: '🔊' },
  invite: { label: 'Invite', icon: '🔗' },
  emoji: { label: 'Emoji', icon: '😀' },
  sticker: { label: 'Sticker', icon: '🏷️' },
  webhook: { label: 'Webhook', icon: '🪝' },
  guild: { label: 'Guild', icon: '🏠' },
  audit: { label: 'Audit', icon: '📘' },
  automod: { label: 'AutoMod', icon: '🚨' },
};
const LOG_CATEGORY_KEYS = Object.freeze(Object.keys(CATEGORY_META));

function nowIso() {
  return new Date().toISOString();
}

function toUnixSeconds(isoValue) {
  const ts = Date.parse(String(isoValue || ''));
  if (!Number.isFinite(ts)) return Math.floor(Date.now() / 1000);
  return Math.floor(ts / 1000);
}

function toDateKey(value) {
  const dt = value ? new Date(value) : new Date();
  if (Number.isNaN(dt.getTime())) {
    return new Date().toISOString().slice(0, 10);
  }
  return dt.toISOString().slice(0, 10);
}

function isValidDayKey(value) {
  const normalized = String(value || '').trim();
  if (!/^\d{4}-\d{2}-\d{2}$/.test(normalized)) return false;
  const timestamp = Date.parse(`${normalized}T00:00:00.000Z`);
  return Number.isFinite(timestamp) && toDateKey(timestamp) === normalized;
}

function truncateText(value, maxLength) {
  const text = String(value ?? '').trim();
  if (!text) return '';
  if (text.length <= maxLength) return text;
  return `${text.slice(0, Math.max(0, maxLength - 3)).trimEnd()}...`;
}

function normalizeInline(value, maxLength = 180) {
  return truncateText(String(value ?? '').replace(/\s+/g, ' '), maxLength);
}

function normalizeBlock(value, maxLength = 500) {
  const text = String(value ?? '')
    .replace(/\r\n/g, '\n')
    .replace(/\r/g, '\n')
    .trim();

  if (!text) return '';
  return truncateText(text, maxLength);
}

function safeArray(value) {
  return Array.isArray(value) ? value : [];
}

function normalizeRetentionDays(value, fallback = 30) {
  const raw = Number(value);
  if (!Number.isFinite(raw)) return fallback;
  const rounded = Math.round(raw);
  return Math.min(MAX_RETENTION_DAYS, Math.max(MIN_RETENTION_DAYS, rounded));
}

function normalizeCategoryKey(value) {
  const normalized = String(value || '').trim().toLowerCase();
  return LOG_CATEGORY_KEYS.includes(normalized) ? normalized : null;
}

function normalizeDisabledCategories(values) {
  const normalized = safeArray(values)
    .map((value) => normalizeCategoryKey(value))
    .filter(Boolean);
  return [...new Set(normalized)].sort();
}

function asAtUser(userTag) {
  const raw = normalizeInline(userTag, 64);
  if (!raw) return '@unknown';
  return raw.startsWith('@') ? raw : `@${raw}`;
}

function formatUserRef(_userId, userTag = null, { bot = false } = {}) {
  const text = asAtUser(userTag);
  return bot ? `${text} [bot]` : text;
}

function formatRoleRef(_roleId, roleName = null) {
  if (!roleName) return '@role';
  const raw = normalizeInline(roleName, 64);
  return raw.startsWith('@') ? raw : `@${raw}`;
}

function simplifyChannelName(channelName) {
  let raw = normalizeInline(channelName, 64);
  if (!raw) return 'channel';

  const colonParts = raw.split(':').map((part) => part.trim()).filter(Boolean);
  if (colonParts.length >= 2) {
    raw = colonParts[colonParts.length - 1];
  }

  raw = raw
    .replace(/^[^\p{L}\p{N}]+/gu, '')
    .replace(/\s{2,}/g, ' ')
    .trim();

  return raw || 'channel';
}

function formatChannelRef(_channelId, channelName = null) {
  const raw = simplifyChannelName(channelName);
  return `#${raw.replace(/^#+/, '')}`;
}

function isSnowflakeLike(value) {
  return /^\d{15,22}$/.test(String(value || ''));
}

function mentionUserById(userId, userTag = null) {
  const id = String(userId || '').trim();
  if (isSnowflakeLike(id)) {
    return `<@${id}>`;
  }
  return asAtUser(userTag);
}

function mentionRoleById(roleId, roleName = null) {
  const id = String(roleId || '').trim();
  if (isSnowflakeLike(id)) {
    return `<@&${id}>`;
  }
  return formatRoleRef(roleId, roleName);
}

function mentionChannelById(channelId, channelName = null) {
  const id = String(channelId || '').trim();
  if (isSnowflakeLike(id)) {
    return `<#${id}>`;
  }
  return formatChannelRef(channelId, channelName);
}

function toActorPayload(user) {
  if (!user?.id) return null;
  return {
    id: user.id,
    tag: user.tag || null,
    bot: user.bot === true,
  };
}

function hasKnownActor(actor) {
  if (!actor) return false;
  return isSnowflakeLike(actor.id) || Boolean(normalizeInline(actor.tag || '', 80));
}

function formatActorMention(actor, fallback = 'unknown') {
  if (!hasKnownActor(actor)) {
    return fallback;
  }
  return mentionUserById(actor.id, actor.tag);
}

function getAuditExtraChannelId(entry) {
  const direct = entry?.extra?.channelId || entry?.extra?.channel_id || null;
  if (direct) return String(direct);

  const nested = entry?.extra?.channel?.id || null;
  if (nested) return String(nested);

  return null;
}

function getAuditExtraCount(entry) {
  const raw = entry?.extra?.count;
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : null;
}

function buildActorResolution(status) {
  if (status === 'matched') return 'audit';
  if (status === 'no_access') return 'audit_unavailable';
  if (status === 'error') return 'audit_error';
  return 'unknown';
}

function buildActorResolutionDetail(status) {
  if (status === 'matched') return null;
  if (status === 'no_access') return 'actor=unknown (audit unavailable)';
  if (status === 'error') return 'actor=unknown (audit lookup failed)';
  return 'actor=unknown';
}

function formatStateDelta(beforeValue, afterValue) {
  const beforeText = normalizeInline(beforeValue, 80) || 'none';
  const afterText = normalizeInline(afterValue, 80) || 'none';
  if (beforeText === afterText) return null;
  return `${beforeText} -> ${afterText}`;
}

function stringifyReason(reason) {
  const text = normalizeInline(reason, 180);
  return text || 'none';
}

function toDisplayName(entity) {
  if (!entity) return 'unknown';
  if (typeof entity === 'string') return normalizeInline(entity, 80) || 'unknown';
  if (typeof entity.name === 'string' && entity.name.trim().length > 0) {
    return normalizeInline(entity.name, 80);
  }
  return 'unknown';
}

function humanizePascalCase(value) {
  const raw = String(value || '').trim();
  if (!raw) return '';
  return raw
    .replace(/([a-z])([A-Z])/g, '$1 $2')
    .replace(/\s+/g, ' ')
    .trim();
}

function formatListWithLimit(values, { max = 5 } = {}) {
  const list = safeArray(values)
    .map((value) => normalizeInline(value, 48))
    .filter(Boolean);
  if (list.length <= max) return list.join(', ');
  const hidden = list.length - max;
  return `${list.slice(0, max).join(', ')}, +${hidden} more`;
}

function formatVoiceChannelLabel(channel) {
  if (!channel) return 'none';
  const name = typeof channel === 'string'
    ? channel
    : channel.name || '';
  const simplified = simplifyChannelName(name);
  return simplified || 'channel';
}

function sanitizeDetailLine(input) {
  const line = normalizeInline(input, 500);
  if (!line) return null;

  const keyValue = /^([a-z0-9_]+)=(.+)$/i.exec(line);
  if (keyValue) {
    const key = keyValue[1].toLowerCase();
    if (key === 'id' || key.endsWith('_id') || key === 'target_id' || key === 'entry_id') {
      return null;
    }
    if (key === 'content' || key === 'before' || key === 'after') {
      return null;
    }
    const humanValue = keyValue[2]
      .replace(/\(\d{15,22}\)/g, '')
      .replace(SNOWFLAKE_RE, '')
      .replace(/\s{2,}/g, ' ')
      .trim();
    if (!humanValue) return null;
    return truncateText(`${key}=${humanValue}`, 130);
  }

  const stripped = line
    .replace(/\(\d{15,22}\)/g, '')
    .replace(SNOWFLAKE_RE, '')
    .replace(/\s{2,}/g, ' ')
    .trim();

  if (!stripped) return null;
  return truncateText(stripped, 130);
}

function buildDisplayDetails(details) {
  const compact = safeArray(details)
    .map((line) => sanitizeDetailLine(line))
    .filter(Boolean);

  if (compact.length === 0) {
    return 'Details: -';
  }

  const selected = compact.slice(0, MAX_RENDERED_DETAILS);
  if (selected.length === 1) {
    return `Details: ${selected[0]}`;
  }
  return ['Details:', ...selected.map((line) => `- ${line}`)].join('\n');
}

function formatActionLabel(action) {
  const raw = String(action || 'event').trim().toLowerCase();
  if (!raw) return 'event';
  if (raw === 'state_update' || raw === 'update') return 'changed';
  if (raw.endsWith('_update')) return 'changed';
  if (raw === 'entry_create') return 'recorded';
  if (raw === 'slash_execute') return 'executed';
  if (raw === 'ban_add') return 'ban';
  if (raw === 'ban_remove') return 'unban';
  return normalizeInline(raw.replace(/_/g, ' '), 48) || 'event';
}

function getDetailSuffix(details) {
  const first = safeArray(details)
    .map((line) => sanitizeDetailLine(line))
    .filter(Boolean)[0];

  if (!first) return '';
  return ` (${truncateText(first, 78)})`;
}

function normalizeAttachmentUrl(attachment) {
  const raw = normalizeInline(attachment?.url || attachment?.proxyURL || attachment?.proxyUrl || '', 600);
  if (!raw) return null;
  try {
    return new URL(raw).toString();
  } catch {
    return null;
  }
}

function serializeAttachmentLine(attachment) {
  if (!attachment) return null;
  if (typeof attachment === 'string') {
    return normalizeInline(attachment, 240);
  }

  const name = normalizeInline(attachment.name || attachment.fileName || attachment.id || '', 100);
  const url = normalizeAttachmentUrl(attachment);
  if (name && url) return `${name}: ${truncateText(url, 240)}`;
  return name || (url ? truncateText(url, 240) : null);
}

function buildMessageDeleteDetail(entry) {
  const target = entry.target || {};
  const metadata = entry.metadata || {};
  const owner = mentionUserById(target.ownerId || metadata.ownerId, target.ownerTag || null);
  const contentValue = normalizeInline(metadata.content || '', 320) || '[empty]';
  const safeContent = contentValue.replace(/"/g, '\'');
  const reasonValue = normalizeInline(metadata.reason || '', 180).replace(/"/g, '\'');
  const files = safeArray(metadata.attachments)
    .map((attachment) => serializeAttachmentLine(attachment))
    .filter(Boolean);

  const parts = [
    `author=${owner}`,
    `content="${safeContent}"`,
  ];

  if (files.length > 0) {
    parts.push(`files=${truncateText(files.join(', '), 520)}`);
  }
  if (reasonValue) {
    parts.push(`reason="${reasonValue}"`);
  }

  return truncateText(parts.join('; '), 980);
}

function buildCompactLogLine(entry, icon) {
  const actor = formatActorMention(entry.actor);
  const actorKnown = hasKnownActor(entry.actor);
  const target = entry.target || {};
  const location = entry.location || {};
  const metadata = entry.metadata || {};
  const category = String(entry.category || '').toLowerCase();
  const action = String(entry.action || '').toLowerCase();
  const detailSuffix = getDetailSuffix(entry.details);

  if (category === 'voice') {
    const from = mentionChannelById(metadata.beforeChannel, null);
    const to = mentionChannelById(metadata.afterChannel || location.channelId, location.channelName || null);
    if (action === 'join') {
      return `${icon} ${actor} joined to ${to}`;
    }
    if (action === 'leave') {
      return `${icon} ${actor} left from ${from}`;
    }
    if (action === 'move') {
      const member = mentionUserById(target.id || metadata.userId, target.tag || null);
      if (actorKnown) {
        return `${icon} ${actor} moved ${member} from ${from} to ${to}`;
      }
      return `${icon} ${member} moved from ${from} to ${to} (actor unknown)`;
    }
    const where = mentionChannelById(location.channelId, location.channelName || null);
    if (actorKnown) {
      return `${icon} ${actor} updated voice in ${where}${detailSuffix}`;
    }
    return `${icon} voice updated in ${where} (actor unknown)${detailSuffix}`;
  }

  if (category === 'role') {
    const role = mentionRoleById(target.id, target.name || null);
    if (action === 'create') {
      return actorKnown
        ? `${icon} ${actor} created ${role}`
        : `${icon} created ${role} (actor unknown)`;
    }
    if (action === 'delete') {
      return actorKnown
        ? `${icon} ${actor} deleted ${role}`
        : `${icon} deleted ${role} (actor unknown)`;
    }
    return actorKnown
      ? `${icon} ${actor} updated ${role}${detailSuffix}`
      : `${icon} updated ${role} (actor unknown)${detailSuffix}`;
  }

  if (category === 'command') {
    const commandName = target.name ? `/${target.name}` : '/unknown';
    const where = mentionChannelById(location.channelId, location.channelName || null);
    return `${icon} ${actor} executed ${commandName} in ${where}`;
  }

  if (category === 'message') {
    const where = mentionChannelById(location.channelId, location.channelName || null);
    if (action === 'delete') {
      const detail = buildMessageDeleteDetail(entry);
      return actorKnown
        ? `${icon} ${actor} deleted message in ${where} (${detail})`
        : `${icon} deleted message in ${where} (actor=unknown; ${detail})`;
    }
    if (action === 'bulk_delete') {
      return actorKnown
        ? `${icon} ${actor} bulk-deleted messages in ${where}`
        : `${icon} bulk-deleted messages in ${where} (actor unknown)`;
    }
    if (action === 'edit' || action === 'update') {
      return `${icon} ${actor} edited message in ${where}`;
    }
    return `${icon} ${actor} updated message in ${where}`;
  }

  if (category === 'member') {
    const member = mentionUserById(target.id, target.tag || null);
    if (action === 'join') {
      return `${icon} ${member} joined server`;
    }
    if (action === 'leave') {
      return `${icon} ${member} left server`;
    }
    if (action === 'remove') {
      return `${icon} ${member} left or was removed from server (actor unknown)`;
    }
    if (action === 'kick') {
      return actorKnown
        ? `${icon} ${member} was kicked by ${actor}`
        : `${icon} ${member} was kicked (actor unknown)`;
    }
    if (action === 'ban') {
      return actorKnown
        ? `${icon} ${member} was banned by ${actor}`
        : `${icon} ${member} was banned (actor unknown)`;
    }
    return actorKnown
      ? `${icon} ${actor} updated ${member}${detailSuffix}`
      : `${icon} updated ${member} (actor unknown)${detailSuffix}`;
  }

  if (category === 'moderation') {
    const member = mentionUserById(target.id, target.tag || null);
    if (action === 'ban_add') {
      return actorKnown
        ? `${icon} ${actor} banned ${member}`
        : `${icon} banned ${member} (actor unknown)`;
    }
    if (action === 'ban_remove') {
      return actorKnown
        ? `${icon} ${actor} unbanned ${member}`
        : `${icon} unbanned ${member} (actor unknown)`;
    }
  }

  if (category === 'channel') {
    const channel = mentionChannelById(target.id || location.channelId, target.name || location.channelName || null);
    if (action === 'create') {
      return actorKnown
        ? `${icon} ${actor} created ${channel}`
        : `${icon} created ${channel} (actor unknown)`;
    }
    if (action === 'delete') {
      return actorKnown
        ? `${icon} ${actor} deleted ${channel}`
        : `${icon} deleted ${channel} (actor unknown)`;
    }
    return actorKnown
      ? `${icon} ${actor} updated ${channel}${detailSuffix}`
      : `${icon} updated ${channel} (actor unknown)${detailSuffix}`;
  }

  const actionLabel = formatActionLabel(entry.action);
  return actorKnown
    ? `${icon} ${actor} ${actionLabel}${detailSuffix}`
    : `${icon} ${actionLabel} (actor unknown)${detailSuffix}`;
}

class FullLogs {
  constructor(client, options = {}) {
    this.client = client;
    this.logger = options.logger || null;
    this.stateStore = options.stateStore || null;
    this.logsDir = options.logsDir || path.join(process.cwd(), 'logs', 'server');

    this.listeners = [];
    this.deliveryQueue = new Map();
    this.writeQueue = new Map();
    this.writeBuffers = new Map();
    this.writeFlushTimers = new Map();
    this.guildDirReady = new Map();
    this.auditCache = new Map();
    this.auditFetchInflight = new Map();
    this.retentionTimer = null;
    this.retentionSweepPromise = null;
    this.started = false;
  }

  start() {
    if (this.started) return;
    this.started = true;

    void fsp.mkdir(this.logsDir, { recursive: true }).catch((error) => {
      this.logger?.warn?.(`FullLogs mkdir error (${this.logsDir}): ${error.message}`);
    });
    this.bindListeners();
    this.startRetentionSweep();
    void this.runRetentionSweep('startup');
    this.logger?.info?.('Full logs module started.');
  }

  stop() {
    if (!this.started) return;
    this.started = false;

    for (const [event, handler] of this.listeners) {
      this.client.off(event, handler);
    }
    this.listeners = [];

    if (this.retentionTimer) {
      clearInterval(this.retentionTimer);
      this.retentionTimer = null;
    }

    void this.flushAllWriteBuffers().catch((error) => {
      this.logger?.warn?.(`FullLogs flush on stop error: ${error.message}`);
    });
    for (const timer of this.writeFlushTimers.values()) {
      clearTimeout(timer);
    }
    this.writeFlushTimers.clear();
    this.auditCache.clear();
    this.auditFetchInflight.clear();
    this.logger?.info?.('Full logs module stopped.');
  }

  sendTestLog(guildId, actorUser) {
    return this.logGuildEvent(guildId, {
      category: 'system',
      action: 'test',
      force: true,
      actor: actorUser
        ? {
          id: actorUser.id,
          tag: actorUser.tag || null,
          bot: actorUser.bot === true,
        }
        : null,
      target: {
        kind: 'guild',
        id: guildId,
      },
      summary: 'Manual test entry from /logs mode:test.',
      details: [
        `runtime_node=${process.versions.node}`,
        `components_v2=${COMPONENTS_V2_FLAG}`,
      ],
      metadata: {
        source: 'command:test',
      },
    });
  }

  bindListeners() {
    this.attach('interactionCreate', async (interaction) => {
      if (!interaction.isChatInputCommand()) return;
      if (!interaction.inGuild()) return;
      const user = interaction.user;
      if (!user) return;

      await this.logGuildEvent(interaction.guildId, {
        category: 'command',
        action: 'slash_execute',
        actor: {
          id: user.id,
          tag: user.tag || null,
          bot: user.bot === true,
        },
        target: {
          kind: 'command',
          name: interaction.commandName,
        },
        location: {
          channelId: interaction.channelId || null,
          channelName: interaction.channel?.name || null,
        },
        summary: `/${interaction.commandName} executed`,
        details: [
          `command=/${interaction.commandName}`,
          `channel=#${interaction.channel?.name || 'unknown'}`,
          `interaction_id=${interaction.id}`,
        ],
        metadata: {
          commandName: interaction.commandName,
          channelId: interaction.channelId || null,
          interactionId: interaction.id,
        },
      });
    });

    this.attach('messageUpdate', async (oldMessage, newMessage) => {
      const guildId = newMessage?.guildId || oldMessage?.guildId;
      if (!guildId) return;

      const author = newMessage?.author || oldMessage?.author || null;
      if (author?.id === this.client.user?.id) return;

      const before = normalizeBlock(oldMessage?.content || '', MAX_DETAIL_VALUE_LENGTH);
      const after = normalizeBlock(newMessage?.content || '', MAX_DETAIL_VALUE_LENGTH);
      const changed = before !== after;

      await this.logGuildEvent(guildId, {
        category: 'message',
        action: 'edit',
        actor: author
          ? {
            id: author.id,
            tag: author.tag || null,
            bot: author.bot === true,
          }
          : null,
        target: {
          kind: 'message',
          id: newMessage?.id || oldMessage?.id || null,
        },
        location: {
          channelId: newMessage?.channelId || oldMessage?.channelId || null,
          channelName: newMessage?.channel?.name || oldMessage?.channel?.name || null,
        },
        summary: changed
          ? 'Message content edited.'
          : 'Message updated (non-content fields).',
        details: [
          `message_id=${newMessage?.id || oldMessage?.id || 'unknown'}`,
          changed ? `before=${before || '[empty]'}` : null,
          changed ? `after=${after || '[empty]'}` : null,
        ],
        metadata: {
          messageId: newMessage?.id || oldMessage?.id || null,
          channelId: newMessage?.channelId || oldMessage?.channelId || null,
          before,
          after,
          contentChanged: changed,
        },
      });
    });

    this.attach('messageDelete', async (message) => {
      if (!message?.guildId) return;
      const author = message.author || null;
      if (author?.id === this.client.user?.id) return;

      const auditMatch = author?.id
        ? await this.resolveAuditEntry({
          guild: message.guild,
          type: AuditLogEvent.MessageDelete,
          targetId: author.id,
          predicate: (entry) => {
            const channelId = getAuditExtraChannelId(entry);
            const count = getAuditExtraCount(entry);
            if (channelId && String(channelId) !== String(message.channelId || '')) {
              return false;
            }
            if (count !== null && count !== 1) {
              return false;
            }
            return true;
          },
        })
        : { entry: null, status: 'miss' };
      const auditEntry = auditMatch.entry;
      const deleter = auditEntry?.executor || null;
      const content = normalizeBlock(message.content || '', MAX_DETAIL_VALUE_LENGTH);
      const attachments = [...(message.attachments?.values?.() || [])]
        .map((attachment) => ({
          id: attachment?.id || null,
          name: normalizeInline(attachment?.name || '', 120) || null,
          url: normalizeInline(attachment?.url || attachment?.proxyURL || '', 260) || null,
          size: Number.isFinite(Number(attachment?.size)) ? Number(attachment.size) : null,
          contentType: normalizeInline(attachment?.contentType || '', 100) || null,
        }))
        .filter((attachment) => attachment.name || attachment.url || attachment.id);

      await this.logGuildEvent(message.guildId, {
        category: 'message',
        action: 'delete',
        actor: toActorPayload(deleter),
        target: {
          kind: 'message',
          id: message.id || null,
          ownerId: author?.id || null,
          ownerTag: author?.tag || null,
        },
        location: {
          channelId: message.channelId || null,
          channelName: message.channel?.name || null,
        },
        summary: deleter
          ? `Message deleted by moderator (${deleter.tag || 'moderator'}).`
          : 'Message deleted. Actor unknown.',
        details: [
          `message_id=${message.id || 'unknown'}`,
          `author=${author ? `${author.tag || author.id} (${author.id})` : 'unknown'}`,
          content ? `content=${content}` : 'content=[empty]',
          attachments.length > 0
            ? `attachments=${attachments.map((attachment) => attachment.name || attachment.url || attachment.id).join(', ')}`
            : 'attachments=[none]',
          buildActorResolutionDetail(auditMatch.status),
          auditEntry?.reason ? `reason=${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          messageId: message.id || null,
          channelId: message.channelId || null,
          ownerId: author?.id || null,
          ownerTag: author?.tag || null,
          deletedBy: deleter?.id || null,
          actorResolution: buildActorResolution(auditMatch.status),
          reason: auditEntry?.reason || null,
          content,
          attachments,
        },
      });
    });

    this.attach('messageDeleteBulk', async (messages, channel) => {
      const first = messages.first();
      const guild = channel?.guild || first?.guild || null;
      const guildId = guild?.id || first?.guildId || null;
      if (!guildId) return;

      const auditEntry = await this.fetchRecentAuditEntry(guild, AuditLogEvent.MessageBulkDelete);
      const executor = auditEntry?.executor || null;

      await this.logGuildEvent(guildId, {
        category: 'message',
        action: 'bulk_delete',
        actor: executor
          ? {
            id: executor.id,
            tag: executor.tag || null,
            bot: executor.bot === true,
          }
          : null,
        target: {
          kind: 'channel',
          id: channel?.id || null,
          name: channel?.name || null,
        },
        location: {
          channelId: channel?.id || null,
          channelName: channel?.name || null,
        },
        summary: `${messages.size} messages deleted in bulk.`,
        details: [
          `count=${messages.size}`,
          `channel=#${channel?.name || 'unknown'}`,
          auditEntry?.reason ? `reason=${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          count: messages.size,
          channelId: channel?.id || null,
          deletedBy: executor?.id || null,
          reason: auditEntry?.reason || null,
        },
      });
    });

    this.attach('messageReactionAdd', async (reaction, user) => {
      const guildId = reaction?.message?.guildId || null;
      if (!guildId || !user) return;
      if (user.id === this.client.user?.id) return;

      await this.logGuildEvent(guildId, {
        category: 'reaction',
        action: 'add',
        actor: {
          id: user.id,
          tag: user.tag || null,
          bot: user.bot === true,
        },
        target: {
          kind: 'message',
          id: reaction.message.id || null,
        },
        location: {
          channelId: reaction.message.channelId || null,
          channelName: reaction.message.channel?.name || null,
        },
        summary: `Reaction added: ${reaction.emoji?.toString() || '[emoji]'}.`,
        details: [
          `emoji=${reaction.emoji?.identifier || reaction.emoji?.name || 'unknown'}`,
          `message_id=${reaction.message.id || 'unknown'}`,
        ],
        metadata: {
          emoji: reaction.emoji?.identifier || reaction.emoji?.name || null,
          channelId: reaction.message.channelId || null,
          messageId: reaction.message.id || null,
        },
      });
    });

    this.attach('messageReactionRemove', async (reaction, user) => {
      const guildId = reaction?.message?.guildId || null;
      if (!guildId || !user) return;
      if (user.id === this.client.user?.id) return;

      await this.logGuildEvent(guildId, {
        category: 'reaction',
        action: 'remove',
        actor: {
          id: user.id,
          tag: user.tag || null,
          bot: user.bot === true,
        },
        target: {
          kind: 'message',
          id: reaction.message.id || null,
        },
        location: {
          channelId: reaction.message.channelId || null,
          channelName: reaction.message.channel?.name || null,
        },
        summary: `Reaction removed: ${reaction.emoji?.toString() || '[emoji]'}.`,
        details: [
          `emoji=${reaction.emoji?.identifier || reaction.emoji?.name || 'unknown'}`,
          `message_id=${reaction.message.id || 'unknown'}`,
        ],
        metadata: {
          emoji: reaction.emoji?.identifier || reaction.emoji?.name || null,
          channelId: reaction.message.channelId || null,
          messageId: reaction.message.id || null,
        },
      });
    });

    this.attach('guildMemberAdd', async (member) => {
      if (!member?.guild?.id || !member.user) return;
      await this.logGuildEvent(member.guild.id, {
        category: 'member',
        action: 'join',
        actor: {
          id: member.user.id,
          tag: member.user.tag || null,
          bot: member.user.bot === true,
        },
        target: {
          kind: 'member',
          id: member.id,
          tag: member.user.tag || null,
        },
        summary: 'Member joined the server.',
        details: [
          `account_created=<t:${Math.floor(member.user.createdTimestamp / 1000)}:F>`,
          `member_count=${member.guild.memberCount}`,
        ],
        metadata: {
          userId: member.id,
          memberCount: member.guild.memberCount,
        },
      });
    });

    this.attach('guildMemberRemove', async (member) => {
      if (!member?.guild?.id || !member.user) return;
      const [kickMatch, banMatch] = await Promise.all([
        this.resolveAuditEntry({
          guild: member.guild,
          type: AuditLogEvent.MemberKick,
          targetId: member.id,
        }),
        this.resolveAuditEntry({
          guild: member.guild,
          type: AuditLogEvent.MemberBanAdd,
          targetId: member.id,
        }),
      ]);

      const auditEntry = banMatch.entry || kickMatch.entry || null;
      const action = banMatch.entry
        ? 'ban'
        : (kickMatch.entry
          ? 'kick'
          : ((banMatch.status === 'miss' && kickMatch.status === 'miss') ? 'leave' : 'remove'));
      const actor = auditEntry?.executor || null;
      const actorStatus = auditEntry
        ? 'matched'
        : (banMatch.status !== 'miss' ? banMatch.status : kickMatch.status);

      await this.logGuildEvent(member.guild.id, {
        category: 'member',
        action,
        actor: toActorPayload(actor),
        target: {
          kind: 'member',
          id: member.id,
          tag: member.user.tag || null,
        },
        summary: banMatch.entry
          ? 'Member banned.'
          : (kickMatch.entry
            ? 'Member kicked.'
            : (action === 'leave'
              ? 'Member left the server.'
              : 'Member left or was removed from the server. Actor unknown.')),
        details: [
          `user=${member.user.tag || member.user.id} (${member.id})`,
          buildActorResolutionDetail(actorStatus),
          auditEntry?.reason ? `reason=${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          userId: member.id,
          reason: auditEntry?.reason || null,
          actorId: actor?.id || null,
          actorResolution: buildActorResolution(actorStatus),
          action,
        },
      });
    });

    this.attach('guildMemberUpdate', async (oldMember, newMember) => {
      const guildId = newMember?.guild?.id || oldMember?.guild?.id;
      if (!guildId || !newMember?.user) return;

      const nickDelta = formatStateDelta(oldMember?.nickname || '', newMember?.nickname || '');
      const oldRoles = new Set([...(oldMember?.roles?.cache?.keys?.() || [])].map(String));
      const newRoles = new Set([...(newMember?.roles?.cache?.keys?.() || [])].map(String));
      const addedRoles = [...newRoles].filter((roleId) => !oldRoles.has(roleId));
      const removedRoles = [...oldRoles].filter((roleId) => !newRoles.has(roleId));

      if (!nickDelta && addedRoles.length === 0 && removedRoles.length === 0) {
        return;
      }

      const details = [];
      if (nickDelta) details.push(`nickname=${nickDelta}`);
      if (addedRoles.length > 0) {
        details.push(`roles_added=${addedRoles
          .map((roleId) => formatRoleRef(roleId, newMember.guild.roles.cache.get(roleId)?.name || null))
          .join(', ')}`);
      }
      if (removedRoles.length > 0) {
        details.push(`roles_removed=${removedRoles
          .map((roleId) => formatRoleRef(roleId, oldMember.guild.roles.cache.get(roleId)?.name || null))
          .join(', ')}`);
      }

      await this.logGuildEvent(guildId, {
        category: 'member',
        action: 'update',
        actor: {
          id: newMember.user.id,
          tag: newMember.user.tag || null,
          bot: newMember.user.bot === true,
        },
        target: {
          kind: 'member',
          id: newMember.id,
          tag: newMember.user.tag || null,
        },
        summary: 'Member profile/roles updated.',
        details,
        metadata: {
          userId: newMember.id,
          nicknameDelta: nickDelta,
          addedRoles,
          removedRoles,
        },
      });
    });

    this.attach('guildBanAdd', async (ban) => {
      if (!ban?.guild?.id || !ban.user) return;
      const auditMatch = await this.resolveAuditEntry({
        guild: ban.guild,
        type: AuditLogEvent.MemberBanAdd,
        targetId: ban.user.id,
      });
      const auditEntry = auditMatch.entry;
      const actor = auditEntry?.executor || null;

      await this.logGuildEvent(ban.guild.id, {
        category: 'moderation',
        action: 'ban_add',
        actor: toActorPayload(actor),
        target: {
          kind: 'member',
          id: ban.user.id,
          tag: ban.user.tag || null,
        },
        summary: actor
          ? `Ban added for ${ban.user.tag || 'member'}.`
          : `Ban added for ${ban.user.tag || 'member'}. Actor unknown.`,
        details: [
          `target=${ban.user.tag || ban.user.id} (${ban.user.id})`,
          buildActorResolutionDetail(auditMatch.status),
          auditEntry?.reason ? `reason=${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          userId: ban.user.id,
          actorId: actor?.id || null,
          actorResolution: buildActorResolution(auditMatch.status),
          reason: auditEntry?.reason || null,
        },
      });
    });

    this.attach('guildBanRemove', async (ban) => {
      if (!ban?.guild?.id || !ban.user) return;
      const auditMatch = await this.resolveAuditEntry({
        guild: ban.guild,
        type: AuditLogEvent.MemberBanRemove,
        targetId: ban.user.id,
      });
      const auditEntry = auditMatch.entry;
      const actor = auditEntry?.executor || null;

      await this.logGuildEvent(ban.guild.id, {
        category: 'moderation',
        action: 'ban_remove',
        actor: toActorPayload(actor),
        target: {
          kind: 'member',
          id: ban.user.id,
          tag: ban.user.tag || null,
        },
        summary: actor
          ? `Ban removed for ${ban.user.tag || 'member'}.`
          : `Ban removed for ${ban.user.tag || 'member'}. Actor unknown.`,
        details: [
          `target=${ban.user.tag || ban.user.id} (${ban.user.id})`,
          buildActorResolutionDetail(auditMatch.status),
          auditEntry?.reason ? `reason=${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          userId: ban.user.id,
          actorId: actor?.id || null,
          actorResolution: buildActorResolution(auditMatch.status),
          reason: auditEntry?.reason || null,
        },
      });
    });

    this.attach('channelCreate', async (channel) => {
      const guildId = channel?.guild?.id;
      if (!guildId) return;

      const auditEntry = await this.fetchAuditEntryByTarget({
        guild: channel.guild,
        type: AuditLogEvent.ChannelCreate,
        targetId: channel.id,
      });

      await this.logGuildEvent(guildId, {
        category: 'channel',
        action: 'create',
        actor: auditEntry?.executor
          ? {
            id: auditEntry.executor.id,
            tag: auditEntry.executor.tag || null,
            bot: auditEntry.executor.bot === true,
          }
          : null,
        target: {
          kind: 'channel',
          id: channel.id,
          name: channel.name || null,
        },
        summary: `Channel created: #${toDisplayName(channel)}.`,
        details: [
          `channel_id=${channel.id}`,
          `type=${channel.type}`,
          `parent_id=${channel.parentId || 'none'}`,
        ],
        metadata: {
          channelId: channel.id,
          type: channel.type,
          parentId: channel.parentId || null,
          actorId: auditEntry?.executor?.id || null,
        },
      });
    });

    this.attach('channelDelete', async (channel) => {
      const guildId = channel?.guild?.id;
      if (!guildId) return;

      const auditMatch = await this.resolveAuditEntry({
        guild: channel.guild,
        type: AuditLogEvent.ChannelDelete,
        targetId: channel.id,
      });
      const auditEntry = auditMatch.entry;
      await this.logGuildEvent(guildId, {
        category: 'channel',
        action: 'delete',
        actor: toActorPayload(auditEntry?.executor || null),
        target: {
          kind: 'channel',
          id: channel.id,
          name: channel.name || null,
        },
        summary: auditEntry?.executor
          ? `Channel deleted: #${toDisplayName(channel)}.`
          : `Channel deleted: #${toDisplayName(channel)}. Actor unknown.`,
        details: [
          `channel_id=${channel.id}`,
          `type=${channel.type}`,
          buildActorResolutionDetail(auditMatch.status),
          auditEntry?.reason ? `reason=${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          channelId: channel.id,
          actorId: auditEntry?.executor?.id || null,
          actorResolution: buildActorResolution(auditMatch.status),
          reason: auditEntry?.reason || null,
        },
      });
    });

    this.attach('channelUpdate', async (oldChannel, newChannel) => {
      const guildId = newChannel?.guild?.id || oldChannel?.guild?.id;
      if (!guildId) return;

      const details = [];
      const nameDelta = formatStateDelta(oldChannel?.name || '', newChannel?.name || '');
      if (nameDelta) details.push(`name=${nameDelta}`);

      const topicDelta = formatStateDelta(oldChannel?.topic || '', newChannel?.topic || '');
      if (topicDelta) details.push(`topic=${topicDelta}`);

      const nsfwDelta = formatStateDelta(oldChannel?.nsfw, newChannel?.nsfw);
      if (nsfwDelta) details.push(`nsfw=${nsfwDelta}`);

      const slowmodeDelta = formatStateDelta(
        oldChannel?.rateLimitPerUser,
        newChannel?.rateLimitPerUser,
      );
      if (slowmodeDelta) details.push(`slowmode_sec=${slowmodeDelta}`);

      const parentDelta = formatStateDelta(oldChannel?.parentId || '', newChannel?.parentId || '');
      if (parentDelta) details.push(`parent_id=${parentDelta}`);

      if (details.length === 0) return;

      const auditEntry = await this.fetchAuditEntryByTarget({
        guild: newChannel.guild,
        type: AuditLogEvent.ChannelUpdate,
        targetId: newChannel.id,
      });

      await this.logGuildEvent(guildId, {
        category: 'channel',
        action: 'update',
        actor: auditEntry?.executor
          ? {
            id: auditEntry.executor.id,
            tag: auditEntry.executor.tag || null,
            bot: auditEntry.executor.bot === true,
          }
          : null,
        target: {
          kind: 'channel',
          id: newChannel.id,
          name: newChannel.name || null,
        },
        summary: `Channel updated: #${toDisplayName(newChannel)}.`,
        details,
        metadata: {
          channelId: newChannel.id,
          actorId: auditEntry?.executor?.id || null,
          reason: auditEntry?.reason || null,
          changes: details,
        },
      });
    });

    this.attach('threadCreate', async (thread) => {
      if (!thread?.guild?.id) return;
      await this.logGuildEvent(thread.guild.id, {
        category: 'thread',
        action: 'create',
        actor: thread.ownerId
          ? { id: thread.ownerId, tag: null, bot: false }
          : null,
        target: {
          kind: 'thread',
          id: thread.id,
          name: thread.name || null,
        },
        location: {
          channelId: thread.parentId || null,
          channelName: thread.parent?.name || null,
        },
        summary: `Thread created: ${toDisplayName(thread)}.`,
        details: [
          `thread_id=${thread.id}`,
          `parent_id=${thread.parentId || 'unknown'}`,
          `archived=${thread.archived ? 'true' : 'false'}`,
        ],
        metadata: {
          threadId: thread.id,
          parentId: thread.parentId || null,
        },
      });
    });

    this.attach('threadDelete', async (thread) => {
      if (!thread?.guild?.id) return;
      await this.logGuildEvent(thread.guild.id, {
        category: 'thread',
        action: 'delete',
        target: {
          kind: 'thread',
          id: thread.id,
          name: thread.name || null,
        },
        location: {
          channelId: thread.parentId || null,
          channelName: thread.parent?.name || null,
        },
        summary: `Thread deleted: ${toDisplayName(thread)}.`,
        details: [
          `thread_id=${thread.id}`,
          `parent_id=${thread.parentId || 'unknown'}`,
        ],
        metadata: {
          threadId: thread.id,
          parentId: thread.parentId || null,
        },
      });
    });

    this.attach('threadUpdate', async (oldThread, newThread) => {
      const guildId = newThread?.guild?.id || oldThread?.guild?.id;
      if (!guildId) return;

      const details = [];
      const nameDelta = formatStateDelta(oldThread?.name || '', newThread?.name || '');
      if (nameDelta) details.push(`name=${nameDelta}`);
      const archiveDelta = formatStateDelta(oldThread?.archived, newThread?.archived);
      if (archiveDelta) details.push(`archived=${archiveDelta}`);
      const lockDelta = formatStateDelta(oldThread?.locked, newThread?.locked);
      if (lockDelta) details.push(`locked=${lockDelta}`);
      if (details.length === 0) return;

      await this.logGuildEvent(guildId, {
        category: 'thread',
        action: 'update',
        target: {
          kind: 'thread',
          id: newThread.id,
          name: newThread.name || null,
        },
        location: {
          channelId: newThread.parentId || null,
          channelName: newThread.parent?.name || null,
        },
        summary: `Thread updated: ${toDisplayName(newThread)}.`,
        details,
        metadata: {
          threadId: newThread.id,
          parentId: newThread.parentId || null,
          changes: details,
        },
      });
    });

    this.attach('roleCreate', async (role) => {
      if (!role?.guild?.id) return;
      const auditEntry = await this.fetchAuditEntryByTarget({
        guild: role.guild,
        type: AuditLogEvent.RoleCreate,
        targetId: role.id,
      });

      await this.logGuildEvent(role.guild.id, {
        category: 'role',
        action: 'create',
        actor: auditEntry?.executor
          ? {
            id: auditEntry.executor.id,
            tag: auditEntry.executor.tag || null,
            bot: auditEntry.executor.bot === true,
          }
          : null,
        target: {
          kind: 'role',
          id: role.id,
          name: role.name || null,
        },
        summary: `Role created: ${role.name || 'role'}.`,
        details: [
          `role_id=${role.id}`,
          `color=${role.hexColor}`,
          `hoist=${role.hoist ? 'true' : 'false'}`,
          `mentionable=${role.mentionable ? 'true' : 'false'}`,
        ],
        metadata: {
          roleId: role.id,
          actorId: auditEntry?.executor?.id || null,
        },
      });
    });

    this.attach('roleDelete', async (role) => {
      if (!role?.guild?.id) return;
      const auditMatch = await this.resolveAuditEntry({
        guild: role.guild,
        type: AuditLogEvent.RoleDelete,
        targetId: role.id,
      });
      const auditEntry = auditMatch.entry;

      await this.logGuildEvent(role.guild.id, {
        category: 'role',
        action: 'delete',
        actor: toActorPayload(auditEntry?.executor || null),
        target: {
          kind: 'role',
          id: role.id,
          name: role.name || null,
        },
        summary: auditEntry?.executor
          ? `Role deleted: ${role.name || 'role'}.`
          : `Role deleted: ${role.name || 'role'}. Actor unknown.`,
        details: [
          `role_id=${role.id}`,
          `position=${role.position}`,
          buildActorResolutionDetail(auditMatch.status),
          auditEntry?.reason ? `reason=${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          roleId: role.id,
          actorId: auditEntry?.executor?.id || null,
          actorResolution: buildActorResolution(auditMatch.status),
          reason: auditEntry?.reason || null,
        },
      });
    });

    this.attach('roleUpdate', async (oldRole, newRole) => {
      const guildId = newRole?.guild?.id || oldRole?.guild?.id;
      if (!guildId) return;

      const details = [];
      let changeCount = 0;
      const nameDelta = formatStateDelta(oldRole?.name || '', newRole?.name || '');
      if (nameDelta) {
        details.push(`Name: ${nameDelta}`);
        changeCount += 1;
      }

      const colorDelta = formatStateDelta(oldRole?.hexColor || '', newRole?.hexColor || '');
      if (colorDelta) {
        details.push(`Color: ${colorDelta}`);
        changeCount += 1;
      }

      const hoistDelta = formatStateDelta(oldRole?.hoist, newRole?.hoist);
      const mentionableDelta = formatStateDelta(oldRole?.mentionable, newRole?.mentionable);
      if (hoistDelta || mentionableDelta) {
        const displayParts = [];
        if (hoistDelta) {
          displayParts.push(`hoist=${hoistDelta}`);
          changeCount += 1;
        }
        if (mentionableDelta) {
          displayParts.push(`mentionable=${mentionableDelta}`);
          changeCount += 1;
        }
        details.push(`Display: ${displayParts.join(', ')}`);
      }

      const positionDelta = formatStateDelta(oldRole?.position, newRole?.position);
      if (positionDelta) {
        details.push(`Position: ${positionDelta}`);
        changeCount += 1;
      }

      const oldPerms = new Set(oldRole?.permissions?.toArray?.() || []);
      const newPerms = new Set(newRole?.permissions?.toArray?.() || []);
      const addedPerms = [...newPerms]
        .filter((perm) => !oldPerms.has(perm))
        .map((perm) => humanizePascalCase(perm));
      const removedPerms = [...oldPerms]
        .filter((perm) => !newPerms.has(perm))
        .map((perm) => humanizePascalCase(perm));

      if (addedPerms.length > 0) {
        details.push(`Permissions (+): ${formatListWithLimit(addedPerms)}`);
        changeCount += 1;
      }
      if (removedPerms.length > 0) {
        details.push(`Permissions (-): ${formatListWithLimit(removedPerms)}`);
        changeCount += 1;
      }

      if (details.length === 0) return;

      const auditEntry = await this.fetchAuditEntryByTarget({
        guild: newRole.guild,
        type: AuditLogEvent.RoleUpdate,
        targetId: newRole.id,
      });

      await this.logGuildEvent(guildId, {
        category: 'role',
        action: 'update',
        actor: auditEntry?.executor
          ? {
            id: auditEntry.executor.id,
            tag: auditEntry.executor.tag || null,
            bot: auditEntry.executor.bot === true,
          }
          : null,
        target: {
          kind: 'role',
          id: newRole.id,
          name: newRole.name || null,
        },
        summary: `Role updated: ${newRole.name || 'role'} (${changeCount} change${changeCount === 1 ? '' : 's'}).`,
        details: [
          ...details,
          auditEntry?.reason ? `Reason: ${stringifyReason(auditEntry.reason)}` : null,
        ],
        metadata: {
          roleId: newRole.id,
          actorId: auditEntry?.executor?.id || null,
          reason: auditEntry?.reason || null,
          changes: details,
        },
      });
    });

    this.attach('voiceStateUpdate', async (oldState, newState) => {
      const guildId = newState?.guild?.id || oldState?.guild?.id;
      const guild = newState?.guild || oldState?.guild || null;
      const member = newState?.member || oldState?.member || null;
      if (!guildId || !member?.user) return;

      const beforeChannel = oldState?.channelId || null;
      const afterChannel = newState?.channelId || null;
      const beforeChannelName = formatVoiceChannelLabel(oldState?.channel || null);
      const afterChannelName = formatVoiceChannelLabel(newState?.channel || null);

      const details = [];
      let voiceAction = 'state_update';
      if (!beforeChannel && afterChannel) {
        voiceAction = 'join';
      } else if (beforeChannel && !afterChannel) {
        voiceAction = 'leave';
      } else if (beforeChannel && afterChannel && beforeChannel !== afterChannel) {
        voiceAction = 'move';
      }

      if (voiceAction === 'join' || voiceAction === 'leave' || voiceAction === 'move') {
        details.push(`from=${beforeChannelName}`);
        details.push(`to=${afterChannelName}`);
      }

      const muteDelta = formatStateDelta(oldState?.mute, newState?.mute);
      if (muteDelta) details.push(`Server mute: ${muteDelta}`);

      const deafDelta = formatStateDelta(oldState?.deaf, newState?.deaf);
      if (deafDelta) details.push(`Server deaf: ${deafDelta}`);

      const selfMuteDelta = formatStateDelta(oldState?.selfMute, newState?.selfMute);
      if (selfMuteDelta) details.push(`Self mute: ${selfMuteDelta}`);

      const selfDeafDelta = formatStateDelta(oldState?.selfDeaf, newState?.selfDeaf);
      if (selfDeafDelta) details.push(`Self deaf: ${selfDeafDelta}`);

      if (details.length === 0) return;

      let actor = toActorPayload(member.user);
      let actorStatus = 'matched';
      if (voiceAction === 'move' && guild) {
        const moveAuditMatch = await this.resolveAuditEntry({
          guild,
          type: AuditLogEvent.MemberMove,
          targetId: member.id,
          predicate: (entry) => {
            const count = getAuditExtraCount(entry);
            return count === null || count >= 1;
          },
        });
        if (moveAuditMatch.entry?.executor) {
          actor = toActorPayload(moveAuditMatch.entry.executor);
          actorStatus = 'matched';
        } else {
          actor = null;
          actorStatus = moveAuditMatch.status;
        }
      }

      await this.logGuildEvent(guildId, {
        category: 'voice',
        action: voiceAction,
        actor,
        target: {
          kind: 'member',
          id: member.id,
          tag: member.user.tag || null,
        },
        location: {
          channelId: afterChannel || beforeChannel || null,
          channelName: voiceAction === 'move'
            ? `${beforeChannelName} -> ${afterChannelName}`
            : (afterChannelName !== 'none' ? afterChannelName : beforeChannelName),
        },
        summary: voiceAction === 'join'
          ? 'User joined voice channel.'
          : (voiceAction === 'leave'
            ? 'User left voice channel.'
            : (voiceAction === 'move'
              ? (actor ? 'User moved between voice channels.' : 'User moved between voice channels. Actor unknown.')
              : `Voice state changed for ${member.user.tag || 'member'}.`)),
        details: [
          ...details,
          voiceAction === 'move' ? buildActorResolutionDetail(actorStatus) : null,
        ],
        metadata: {
          userId: member.id,
          beforeChannel,
          afterChannel,
          changes: details,
          actorResolution: voiceAction === 'move' ? buildActorResolution(actorStatus) : 'event',
        },
      });
    });

    this.attach('inviteCreate', async (invite) => {
      if (!invite?.guild?.id) return;
      await this.logGuildEvent(invite.guild.id, {
        category: 'invite',
        action: 'create',
        actor: invite.inviter
          ? {
            id: invite.inviter.id,
            tag: invite.inviter.tag || null,
            bot: invite.inviter.bot === true,
          }
          : null,
        target: {
          kind: 'invite',
          id: invite.code || null,
        },
        location: {
          channelId: invite.channelId || null,
          channelName: invite.channel?.name || null,
        },
        summary: `Invite created: ${invite.code || 'unknown'}.`,
        details: [
          `code=${invite.code || 'unknown'}`,
          `max_uses=${invite.maxUses || 0}`,
          `temporary=${invite.temporary ? 'true' : 'false'}`,
          `expires_at=${invite.expiresAt ? invite.expiresAt.toISOString() : 'never'}`,
        ],
        metadata: {
          code: invite.code || null,
          channelId: invite.channelId || null,
          inviterId: invite.inviter?.id || null,
        },
      });
    });

    this.attach('inviteDelete', async (invite) => {
      if (!invite?.guild?.id) return;
      await this.logGuildEvent(invite.guild.id, {
        category: 'invite',
        action: 'delete',
        target: {
          kind: 'invite',
          id: invite.code || null,
        },
        location: {
          channelId: invite.channelId || null,
          channelName: invite.channel?.name || null,
        },
        summary: `Invite deleted: ${invite.code || 'unknown'}.`,
        details: [
          `code=${invite.code || 'unknown'}`,
          `channel=#${invite.channel?.name || 'unknown'}`,
        ],
        metadata: {
          code: invite.code || null,
          channelId: invite.channelId || null,
        },
      });
    });

    this.attach('emojiCreate', async (emoji) => {
      if (!emoji?.guild?.id) return;
      await this.logGuildEvent(emoji.guild.id, {
        category: 'emoji',
        action: 'create',
        target: {
          kind: 'emoji',
          id: emoji.id || null,
          name: emoji.name || null,
        },
        summary: `Emoji created: ${emoji.name || 'emoji'}.`,
        details: [
          `emoji_id=${emoji.id || 'unicode'}`,
          `animated=${emoji.animated ? 'true' : 'false'}`,
        ],
        metadata: {
          emojiId: emoji.id || null,
          name: emoji.name || null,
        },
      });
    });

    this.attach('emojiDelete', async (emoji) => {
      if (!emoji?.guild?.id) return;
      await this.logGuildEvent(emoji.guild.id, {
        category: 'emoji',
        action: 'delete',
        target: {
          kind: 'emoji',
          id: emoji.id || null,
          name: emoji.name || null,
        },
        summary: `Emoji deleted: ${emoji.name || 'emoji'}.`,
        details: [
          `emoji_id=${emoji.id || 'unicode'}`,
        ],
        metadata: {
          emojiId: emoji.id || null,
          name: emoji.name || null,
        },
      });
    });

    this.attach('emojiUpdate', async (oldEmoji, newEmoji) => {
      const guildId = newEmoji?.guild?.id || oldEmoji?.guild?.id;
      if (!guildId) return;

      const nameDelta = formatStateDelta(oldEmoji?.name || '', newEmoji?.name || '');
      if (!nameDelta) return;

      await this.logGuildEvent(guildId, {
        category: 'emoji',
        action: 'update',
        target: {
          kind: 'emoji',
          id: newEmoji?.id || oldEmoji?.id || null,
          name: newEmoji?.name || oldEmoji?.name || null,
        },
        summary: 'Emoji renamed.',
        details: [`name=${nameDelta}`],
        metadata: {
          emojiId: newEmoji?.id || oldEmoji?.id || null,
          nameDelta,
        },
      });
    });

    this.attach('stickerCreate', async (sticker) => {
      if (!sticker?.guild?.id) return;
      await this.logGuildEvent(sticker.guild.id, {
        category: 'sticker',
        action: 'create',
        target: {
          kind: 'sticker',
          id: sticker.id,
          name: sticker.name || null,
        },
        summary: `Sticker created: ${sticker.name || 'sticker'}.`,
        details: [
          `sticker_id=${sticker.id}`,
          `format=${sticker.format}`,
        ],
        metadata: {
          stickerId: sticker.id,
          name: sticker.name || null,
        },
      });
    });

    this.attach('stickerDelete', async (sticker) => {
      if (!sticker?.guild?.id) return;
      await this.logGuildEvent(sticker.guild.id, {
        category: 'sticker',
        action: 'delete',
        target: {
          kind: 'sticker',
          id: sticker.id,
          name: sticker.name || null,
        },
        summary: `Sticker deleted: ${sticker.name || 'sticker'}.`,
        details: [`sticker_id=${sticker.id}`],
        metadata: {
          stickerId: sticker.id,
          name: sticker.name || null,
        },
      });
    });

    this.attach('stickerUpdate', async (oldSticker, newSticker) => {
      const guildId = newSticker?.guild?.id || oldSticker?.guild?.id;
      if (!guildId) return;
      const nameDelta = formatStateDelta(oldSticker?.name || '', newSticker?.name || '');
      const descDelta = formatStateDelta(oldSticker?.description || '', newSticker?.description || '');
      const details = [];
      if (nameDelta) details.push(`name=${nameDelta}`);
      if (descDelta) details.push(`description=${descDelta}`);
      if (details.length === 0) return;

      await this.logGuildEvent(guildId, {
        category: 'sticker',
        action: 'update',
        target: {
          kind: 'sticker',
          id: newSticker.id,
          name: newSticker.name || null,
        },
        summary: `Sticker updated: ${newSticker.name || 'sticker'}.`,
        details,
        metadata: {
          stickerId: newSticker.id,
          changes: details,
        },
      });
    });

    this.attach('webhookUpdate', async (channel) => {
      const guildId = channel?.guild?.id;
      if (!guildId) return;
      await this.logGuildEvent(guildId, {
        category: 'webhook',
        action: 'update',
        target: {
          kind: 'channel',
          id: channel.id,
          name: channel.name || null,
        },
        location: {
          channelId: channel.id,
          channelName: channel.name || null,
        },
        summary: `Webhook list changed in #${toDisplayName(channel)}.`,
        details: [
          `channel_id=${channel.id}`,
        ],
        metadata: {
          channelId: channel.id,
        },
      });
    });

    this.attach('guildUpdate', async (oldGuild, newGuild) => {
      const guildId = newGuild?.id || oldGuild?.id;
      if (!guildId) return;

      const details = [];
      const nameDelta = formatStateDelta(oldGuild?.name || '', newGuild?.name || '');
      if (nameDelta) details.push(`name=${nameDelta}`);

      const iconDelta = formatStateDelta(oldGuild?.icon || '', newGuild?.icon || '');
      if (iconDelta) details.push(`icon_hash=${iconDelta}`);

      const afkDelta = formatStateDelta(oldGuild?.afkChannelId || '', newGuild?.afkChannelId || '');
      if (afkDelta) details.push(`afk_channel=${afkDelta}`);

      const verificationDelta = formatStateDelta(oldGuild?.verificationLevel, newGuild?.verificationLevel);
      if (verificationDelta) details.push(`verification_level=${verificationDelta}`);

      if (details.length === 0) return;

      const auditEntry = await this.fetchRecentAuditEntry(newGuild, AuditLogEvent.GuildUpdate);
      await this.logGuildEvent(guildId, {
        category: 'guild',
        action: 'update',
        actor: auditEntry?.executor
          ? {
            id: auditEntry.executor.id,
            tag: auditEntry.executor.tag || null,
            bot: auditEntry.executor.bot === true,
          }
          : null,
        target: {
          kind: 'guild',
          id: guildId,
          name: newGuild.name || null,
        },
        summary: `Guild updated: ${newGuild.name || 'server'}.`,
        details,
        metadata: {
          guildId,
          actorId: auditEntry?.executor?.id || null,
          reason: auditEntry?.reason || null,
          changes: details,
        },
      });
    });

    this.attach('autoModerationActionExecution', async (execution) => {
      const guildId = execution?.guild?.id || execution?.guildId || null;
      if (!guildId) return;

      await this.logGuildEvent(guildId, {
        category: 'automod',
        action: 'execution',
        actor: execution.user
          ? {
            id: execution.user.id,
            tag: execution.user.tag || null,
            bot: execution.user.bot === true,
          }
          : null,
        target: {
          kind: 'member',
          id: execution.userId || execution.user?.id || null,
          tag: execution.user?.tag || null,
        },
        location: {
          channelId: execution.channelId || null,
          channelName: execution.channel?.name || null,
        },
        summary: `AutoMod action executed: ${execution.action?.type || 'unknown'}.`,
        details: [
          `rule_id=${execution.ruleId || 'unknown'}`,
          `rule_trigger=${execution.ruleTriggerType || 'unknown'}`,
          `action_type=${execution.action?.type || 'unknown'}`,
          execution.matchedKeyword ? `keyword=${normalizeInline(execution.matchedKeyword, 80)}` : null,
          execution.content ? `content=${normalizeBlock(execution.content, MAX_DETAIL_VALUE_LENGTH)}` : null,
        ],
        metadata: {
          ruleId: execution.ruleId || null,
          channelId: execution.channelId || null,
          userId: execution.userId || execution.user?.id || null,
          actionType: execution.action?.type || null,
          matchedKeyword: execution.matchedKeyword || null,
        },
      });
    });
  }

  attach(eventName, asyncHandler) {
    const wrapped = (...args) => {
      Promise.resolve(asyncHandler(...args)).catch((error) => {
        this.logger?.warn?.(`FullLogs handler error (${eventName}): ${error.message}`);
      });
    };
    this.client.on(eventName, wrapped);
    this.listeners.push([eventName, wrapped]);
  }

  getLogsState(guildId) {
    const fallback = {
      enabled: false,
      channelId: null,
      retentionDays: 30,
      forwardMode: 'all',
      includeBots: false,
      disabledCategories: [],
    };
    const current = this.stateStore?.getLogsState
      ? this.stateStore.getLogsState(guildId)
      : {};
    return {
      enabled: current?.enabled === true,
      channelId: current?.channelId ? String(current.channelId) : null,
      retentionDays: normalizeRetentionDays(current?.retentionDays, fallback.retentionDays),
      forwardMode: String(current?.forwardMode || fallback.forwardMode),
      includeBots: current?.includeBots === true,
      disabledCategories: normalizeDisabledCategories(current?.disabledCategories),
    };
  }

  listSupportedCategories() {
    return [...LOG_CATEGORY_KEYS];
  }

  isCategoryEnabled(state, category) {
    const normalized = normalizeCategoryKey(category);
    if (!normalized) return true;
    return !safeArray(state?.disabledCategories).includes(normalized);
  }

  getDiagnostics(guildId = null, guild = null) {
    const normalizedGuildId = guildId ? String(guildId) : null;
    const bufferedWrites = normalizedGuildId
      ? (this.writeBuffers.get(normalizedGuildId)?.length || 0)
      : [...this.writeBuffers.values()].reduce((sum, batch) => sum + (Array.isArray(batch) ? batch.length : 0), 0);

    return {
      started: this.started === true,
      logsDir: this.logsDir,
      logsDirExists: fs.existsSync(this.logsDir),
      state: normalizedGuildId ? this.getLogsState(normalizedGuildId) : null,
      canViewAuditLog: guild ? this.canViewAuditLog(guild) : null,
      deliveryQueueSize: normalizedGuildId
        ? (this.deliveryQueue.has(normalizedGuildId) ? 1 : 0)
        : this.deliveryQueue.size,
      writeQueueSize: normalizedGuildId
        ? (this.writeQueue.has(normalizedGuildId) ? 1 : 0)
        : this.writeQueue.size,
      bufferedWrites,
      auditCacheEntries: this.auditCache.size,
      auditFetchInflight: this.auditFetchInflight.size,
      retentionTimerActive: Boolean(this.retentionTimer),
      retentionSweepActive: Boolean(this.retentionSweepPromise),
    };
  }

  shouldSkipByActor(state, actor) {
    if (!actor) return false;
    if (actor.id && actor.id === this.client.user?.id) return true;
    if (state.includeBots) return false;
    return actor.bot === true;
  }

  async logGuildEvent(guildId, entryInput) {
    if (!guildId) return false;
    const state = this.getLogsState(guildId);
    if (!state.enabled || !state.channelId) {
      return false;
    }

    const timestamp = nowIso();
    const entry = {
      ts: timestamp,
      guildId: String(guildId),
      category: String(entryInput.category || 'system'),
      action: String(entryInput.action || 'event'),
      actor: entryInput.actor || null,
      target: entryInput.target || null,
      location: entryInput.location || null,
      summary: normalizeInline(entryInput.summary || '', MAX_SUMMARY_LENGTH) || 'Event captured.',
      details: safeArray(entryInput.details).filter(Boolean).slice(0, MAX_DETAIL_LINES).map((line) => normalizeInline(line, MAX_DETAIL_VALUE_LENGTH)),
      metadata: entryInput.metadata || {},
    };

    if (entryInput.force !== true && !this.isCategoryEnabled(state, entry.category)) {
      return false;
    }

    if (this.shouldSkipByActor(state, entry.actor)) {
      return false;
    }

    this.writeJsonLine(guildId, entry);
    this.enqueueDelivery(guildId, state.channelId, entry);
    return true;
  }

  writeJsonLine(guildId, entry) {
    try {
      const normalizedGuildId = String(guildId);
      const dateKey = toDateKey(entry.ts);
      const filePath = path.join(this.logsDir, normalizedGuildId, `${dateKey}.jsonl`);
      const current = this.writeBuffers.get(normalizedGuildId) || [];
      current.push({
        filePath,
        line: `${JSON.stringify(entry)}\n`,
      });
      this.writeBuffers.set(normalizedGuildId, current);

      if (this.writeFlushTimers.has(normalizedGuildId)) {
        return;
      }

      const timer = setTimeout(() => {
        this.writeFlushTimers.delete(normalizedGuildId);
        void this.flushGuildWriteBuffer(normalizedGuildId).catch((error) => {
          this.logger?.warn?.(`FullLogs write flush error (${normalizedGuildId}): ${error.message}`);
        });
      }, WRITE_BATCH_DELAY_MS);
      timer.unref?.();
      this.writeFlushTimers.set(normalizedGuildId, timer);
    } catch (error) {
      this.logger?.warn?.(`FullLogs write queue error (${guildId}): ${error.message}`);
    }
  }

  async ensureGuildDir(guildId) {
    const normalizedGuildId = String(guildId);
    const existing = this.guildDirReady.get(normalizedGuildId);
    if (existing) {
      return existing;
    }

    const guildDir = path.join(this.logsDir, normalizedGuildId);
    const pending = fsp.mkdir(guildDir, { recursive: true })
      .catch((error) => {
        this.guildDirReady.delete(normalizedGuildId);
        throw error;
      });
    this.guildDirReady.set(normalizedGuildId, pending);
    return pending;
  }

  enqueueWrite(guildId, asyncWriter) {
    const normalizedGuildId = String(guildId);
    const previous = this.writeQueue.get(normalizedGuildId) || Promise.resolve();
    const next = previous
      .catch(() => {})
      .then(asyncWriter)
      .catch((error) => {
        this.logger?.warn?.(`FullLogs disk write error (${normalizedGuildId}): ${error.message}`);
      })
      .finally(() => {
        if (this.writeQueue.get(normalizedGuildId) === next) {
          this.writeQueue.delete(normalizedGuildId);
        }
      });

    this.writeQueue.set(normalizedGuildId, next);
    return next;
  }

  async flushGuildWriteBuffer(guildId) {
    const normalizedGuildId = String(guildId);
    const buffered = this.writeBuffers.get(normalizedGuildId);
    if (!buffered || buffered.length === 0) {
      return;
    }

    this.writeBuffers.delete(normalizedGuildId);
    await this.enqueueWrite(normalizedGuildId, async () => {
      await this.ensureGuildDir(normalizedGuildId);

      let currentFilePath = null;
      let currentPayload = '';

      for (const item of buffered) {
        if (!currentFilePath) {
          currentFilePath = item.filePath;
          currentPayload = item.line;
          continue;
        }

        if (item.filePath === currentFilePath) {
          currentPayload += item.line;
          continue;
        }

        await fsp.appendFile(currentFilePath, currentPayload, 'utf8');
        currentFilePath = item.filePath;
        currentPayload = item.line;
      }

      if (currentFilePath) {
        await fsp.appendFile(currentFilePath, currentPayload, 'utf8');
      }
    });
  }

  async flushAllWriteBuffers() {
    const guildIds = [...this.writeBuffers.keys()];
    await Promise.all(guildIds.map((guildId) => this.flushGuildWriteBuffer(guildId)));
  }

  async waitForIdle({ guildId = null } = {}) {
    const normalizedGuildId = guildId ? String(guildId) : null;

    while (true) {
      if (normalizedGuildId) {
        if (this.writeFlushTimers.has(normalizedGuildId)) {
          clearTimeout(this.writeFlushTimers.get(normalizedGuildId));
          this.writeFlushTimers.delete(normalizedGuildId);
        }
        await this.flushGuildWriteBuffer(normalizedGuildId);
      } else {
        for (const [pendingGuildId, timer] of this.writeFlushTimers.entries()) {
          clearTimeout(timer);
          this.writeFlushTimers.delete(pendingGuildId);
        }
        await this.flushAllWriteBuffers();
      }

      const pending = normalizedGuildId
        ? [
          this.writeQueue.get(normalizedGuildId),
          this.deliveryQueue.get(normalizedGuildId),
          ...[...this.auditFetchInflight.values()].map((item) => item.promise),
        ].filter(Boolean)
        : [
          ...this.writeQueue.values(),
          ...this.deliveryQueue.values(),
          ...[...this.auditFetchInflight.values()].map((item) => item.promise),
        ].filter(Boolean);

      const hasBufferedWrites = normalizedGuildId
        ? (this.writeBuffers.get(normalizedGuildId)?.length || 0) > 0
        : this.writeBuffers.size > 0;

      if (pending.length === 0 && !hasBufferedWrites) {
        return;
      }

      await Promise.all(pending);
    }
  }

  enqueueDelivery(guildId, channelId, entry) {
    const previous = this.deliveryQueue.get(guildId) || Promise.resolve();
    const next = previous
      .catch(() => {})
      .then(async () => {
        const payload = this.buildV2Payload(entry);
        await this.client.rest.post(Routes.channelMessages(channelId), {
          body: payload,
        });
      })
      .catch((error) => {
        this.logger?.warn?.(`FullLogs delivery error (${guildId}/${channelId}): ${error.message}`);
      })
      .finally(() => {
        if (this.deliveryQueue.get(guildId) === next) {
          this.deliveryQueue.delete(guildId);
        }
      });

    this.deliveryQueue.set(guildId, next);
    return next;
  }

  buildV2Payload(entry) {
    const categoryMeta = CATEGORY_META[entry.category] || CATEGORY_META.system;
    const icon = categoryMeta.icon || '🧩';
    const line = truncateText(buildCompactLogLine(entry, icon), MAX_TEXT_DISPLAY_LENGTH);

    return {
      flags: COMPONENTS_V2_FLAG,
      allowed_mentions: {
        parse: [],
      },
      components: [
        {
          type: 17,
          components: [
            {
              type: 10,
              content: line,
            },
          ],
        },
      ],
    };
  }

  buildFallbackPreviewEntry(guildId) {
    return {
      ts: nowIso(),
      guildId: String(guildId),
      category: 'system',
      action: 'preview',
      actor: null,
      target: {
        kind: 'guild',
        id: guildId,
      },
      location: null,
      summary: 'Preview sample entry.',
      details: [
        'source=preview',
        'note=Brak zapisanych wpisow, pokazuje przykladowy format.',
      ],
      metadata: {
        source: 'preview:fallback',
      },
    };
  }

  getGuildLogsDir(guildId) {
    return path.join(this.logsDir, String(guildId));
  }

  getDayLogPath(guildId, dayKey) {
    return path.join(this.getGuildLogsDir(guildId), `${dayKey}.jsonl`);
  }

  async getLatestSavedEntry(guildId) {
    const guildDir = this.getGuildLogsDir(guildId);
    const files = await fsp.readdir(guildDir).catch((error) => {
      if (error?.code === 'ENOENT') return [];
      throw error;
    });

    const dayFiles = files
      .filter((fileName) => /^\d{4}-\d{2}-\d{2}\.jsonl$/.test(fileName))
      .sort()
      .reverse();

    for (const fileName of dayFiles) {
      const filePath = path.join(guildDir, fileName);
      const raw = await fsp.readFile(filePath, 'utf8').catch((error) => {
        if (error?.code === 'ENOENT') return null;
        throw error;
      });
      if (!raw) continue;

      const lines = raw
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean);

      for (let index = lines.length - 1; index >= 0; index -= 1) {
        try {
          const parsed = JSON.parse(lines[index]);
          if (parsed && typeof parsed === 'object') {
            return {
              entry: parsed,
              dayKey: fileName.replace(/\.jsonl$/i, ''),
            };
          }
        } catch {
          // Ignore malformed lines and continue scanning backwards.
        }
      }
    }

    return null;
  }

  async createPreviewPayload(guildId) {
    await this.waitForIdle({ guildId }).catch(() => {});

    const latest = await this.getLatestSavedEntry(guildId).catch((error) => {
      this.logger?.warn?.(`FullLogs preview read error (${guildId}): ${error.message}`);
      return null;
    });

    const entry = latest?.entry || this.buildFallbackPreviewEntry(guildId);
    const categoryMeta = CATEGORY_META[entry.category] || CATEGORY_META.system;
    const line = truncateText(
      buildCompactLogLine(entry, categoryMeta.icon || '🧩'),
      MAX_TEXT_DISPLAY_LENGTH,
    );

    const note = latest
      ? `Podglad bazuje na ostatnim zapisanym wpisie z dnia \`${latest.dayKey}\`.`
      : 'Brak zapisanych wpisow. Pokazuje przykladowy format loga.';

    return {
      flags: COMPONENTS_V2_FLAG | 64,
      allowed_mentions: {
        parse: [],
      },
      components: [
        {
          type: 17,
          components: [
            {
              type: 10,
              content: '## Logs Preview',
            },
            {
              type: 10,
              content: note,
            },
            {
              type: 10,
              content: line,
            },
          ],
        },
      ],
    };
  }

  async exportDay(guildId, dayKey) {
    const normalizedDay = String(dayKey || '').trim();
    if (!isValidDayKey(normalizedDay)) {
      throw new Error('Niepoprawny format dnia. Uzyj YYYY-MM-DD.');
    }

    await this.waitForIdle({ guildId });
    const filePath = this.getDayLogPath(guildId, normalizedDay);
    const exists = await fsp.access(filePath, fs.constants.F_OK).then(() => true).catch(() => false);
    if (!exists) {
      return null;
    }

    return {
      filePath,
      fileName: `${guildId}-${normalizedDay}.jsonl`,
      dayKey: normalizedDay,
    };
  }

  async purgeLogs(guildId, { day = null, olderThanDays = null } = {}) {
    const normalizedGuildId = String(guildId || '').trim();
    const normalizedDay = day ? String(day).trim() : null;
    const normalizedOlderThanDays = olderThanDays === null || olderThanDays === undefined
      ? null
      : Math.max(1, Math.floor(Number(olderThanDays)));

    if (!normalizedGuildId) {
      throw new Error('Brak guildId dla purge logs.');
    }
    if (normalizedDay && normalizedOlderThanDays !== null) {
      throw new Error('Wybierz purge po dniu albo po starszych plikach, nie oba naraz.');
    }
    if (!normalizedDay && normalizedOlderThanDays === null) {
      throw new Error('Podaj `day` albo `older_than_days`.');
    }

    await this.waitForIdle({ guildId: normalizedGuildId });

    if (normalizedDay) {
      if (!isValidDayKey(normalizedDay)) {
        throw new Error('Niepoprawny format dnia. Uzyj YYYY-MM-DD.');
      }

      const filePath = this.getDayLogPath(normalizedGuildId, normalizedDay);
      const exists = await fsp.access(filePath, fs.constants.F_OK).then(() => true).catch(() => false);
      if (!exists) {
        return {
          mode: 'day',
          removed: 0,
          removedDays: [],
        };
      }

      await fsp.unlink(filePath);
      return {
        mode: 'day',
        removed: 1,
        removedDays: [normalizedDay],
      };
    }

    const guildDir = this.getGuildLogsDir(normalizedGuildId);
    const files = await fsp.readdir(guildDir).catch((error) => {
      if (error?.code === 'ENOENT') return [];
      throw error;
    });

    const todayUtc = new Date();
    todayUtc.setUTCHours(0, 0, 0, 0);
    const cutoffMs = todayUtc.getTime() - (normalizedOlderThanDays * 24 * 60 * 60 * 1000);
    const removedDays = [];

    for (const fileName of files) {
      const match = /^(\d{4}-\d{2}-\d{2})\.jsonl$/.exec(fileName);
      if (!match) continue;

      const dayValue = match[1];
      const fileDate = Date.parse(`${dayValue}T00:00:00.000Z`);
      if (!Number.isFinite(fileDate) || fileDate >= cutoffMs) continue;

      await fsp.unlink(path.join(guildDir, fileName)).catch((error) => {
        if (error?.code !== 'ENOENT') {
          throw error;
        }
      });
      removedDays.push(dayValue);
    }

    return {
      mode: 'older_than_days',
      olderThanDays: normalizedOlderThanDays,
      removed: removedDays.length,
      removedDays: removedDays.sort(),
    };
  }

  renderTarget(target) {
    if (!target) return null;
    const kind = String(target.kind || '').toLowerCase();

    if (kind === 'member' || kind === 'user') {
      return target.tag ? asAtUser(target.tag) : 'member';
    }
    if (kind === 'channel') {
      return formatChannelRef(target.id, target.name || null);
    }
    if (kind === 'role') {
      return formatRoleRef(target.id, target.name || null);
    }
    if (kind === 'message') {
      if (target.ownerTag) {
        return `message (author: ${asAtUser(target.ownerTag)})`;
      }
      return 'message';
    }
    if (kind === 'command') {
      return `/${target.name || 'unknown'}`;
    }
    if (kind === 'invite') {
      return target.name ? `invite ${target.name}` : 'invite';
    }
    if (kind === 'guild') {
      if (target.name) {
        return normalizeInline(target.name, 80);
      }
      return 'server';
    }
    if (kind === 'thread') {
      const threadName = target.name ? normalizeInline(target.name, 80) : 'thread';
      return threadName;
    }
    if (kind === 'emoji' || kind === 'sticker') {
      const name = target.name ? normalizeInline(target.name, 80) : kind;
      return name;
    }
    if (kind === 'audit') {
      return 'audit entry';
    }

    return normalizeInline(kind || 'target', 80);
  }

  canViewAuditLog(guild) {
    return guild?.members?.me?.permissions?.has(PermissionFlagsBits.ViewAuditLog) === true;
  }

  async fetchAuditEntries(guild, type, { limit = 10 } = {}) {
    if (!guild?.id) {
      return { status: 'no_guild', entries: [] };
    }
    if (!this.canViewAuditLog(guild)) {
      return { status: 'no_access', entries: [] };
    }

    const key = `${guild.id}:${type}`;
    const normalizedLimit = Math.max(AUDIT_FETCH_LIMIT, Number(limit) || 0);
    const now = Date.now();
    const cached = this.auditCache.get(key);
    if (cached && now - cached.ts < AUDIT_CACHE_TTL_MS && cached.limit >= normalizedLimit) {
      return {
        status: 'ok',
        entries: (cached.entries || []).slice(0, normalizedLimit),
      };
    }

    const inflight = this.auditFetchInflight.get(key);
    if (inflight && inflight.limit >= normalizedLimit) {
      const inflightResult = await inflight.promise;
      if (inflightResult.status !== 'ok') {
        return inflightResult;
      }
      return {
        status: 'ok',
        entries: (inflightResult.entries || []).slice(0, normalizedLimit),
      };
    }

    const promise = guild.fetchAuditLogs({
      limit: normalizedLimit,
      type,
    }).then((fetched) => {
      const entries = [...fetched.entries.values()]
        .sort((a, b) => b.createdTimestamp - a.createdTimestamp);

      this.auditCache.set(key, {
        ts: Date.now(),
        limit: normalizedLimit,
        entries,
      });

      return {
        status: 'ok',
        entries,
      };
    }).catch((error) => {
      this.logger?.warn?.(`FullLogs audit fetch failed (${guild.id}/${type}): ${error.message}`);
      return { status: 'error', entries: [] };
    }).finally(() => {
      const current = this.auditFetchInflight.get(key);
      if (current?.promise === promise) {
        this.auditFetchInflight.delete(key);
      }
    });

    this.auditFetchInflight.set(key, {
      limit: normalizedLimit,
      promise,
    });

    const result = await promise;
    if (result.status !== 'ok') {
      return result;
    }
    return {
      status: 'ok',
      entries: (result.entries || []).slice(0, normalizedLimit),
    };
  }

  matchAuditEntry(entries, {
    targetId = null,
    predicate = null,
    windowMs = AUDIT_MATCH_WINDOW_MS,
  } = {}) {
    const now = Date.now();
    for (const entry of safeArray(entries)) {
      if (now - entry.createdTimestamp > windowMs) continue;
      if (targetId && String(entry.targetId || '') !== String(targetId)) continue;
      if (typeof predicate === 'function' && !predicate(entry)) continue;
      return entry;
    }
    return null;
  }

  async resolveAuditEntry({
    guild,
    type,
    targetId = null,
    predicate = null,
    limit = 10,
    windowMs = AUDIT_MATCH_WINDOW_MS,
  }) {
    const fetched = await this.fetchAuditEntries(guild, type, { limit });
    if (fetched.status !== 'ok') {
      return {
        entry: null,
        status: fetched.status,
      };
    }

    const matched = this.matchAuditEntry(fetched.entries, {
      targetId,
      predicate,
      windowMs,
    });

    return {
      entry: matched,
      status: matched ? 'matched' : 'miss',
    };
  }

  async fetchRecentAuditEntry(guild, type) {
    const resolved = await this.resolveAuditEntry({
      guild,
      type,
      limit: AUDIT_FETCH_LIMIT,
    });
    return resolved.entry;
  }

  async fetchAuditEntryByTarget({ guild, type, targetId, predicate = null }) {
    if (!guild?.id || !targetId) return null;
    const resolved = await this.resolveAuditEntry({
      guild,
      type,
      targetId,
      predicate,
    });
    return resolved.entry;
  }

  startRetentionSweep() {
    if (this.retentionTimer) {
      clearInterval(this.retentionTimer);
    }

    this.retentionTimer = setInterval(() => {
      void this.runRetentionSweep('interval');
    }, RETENTION_SWEEP_INTERVAL_MS);

    this.retentionTimer.unref?.();
  }

  async runRetentionSweep(trigger) {
    if (this.retentionSweepPromise) {
      return this.retentionSweepPromise;
    }

    const sweep = (async () => {
      try {
        const dirEntries = await fsp.readdir(this.logsDir, { withFileTypes: true }).catch((error) => {
          if (error?.code === 'ENOENT') {
            return [];
          }
          throw error;
        });

        let removed = 0;
        for (const entry of dirEntries) {
          if (!entry.isDirectory()) continue;
          const guildId = entry.name;
          const state = this.getLogsState(guildId);
          removed += await this.pruneGuildFiles(guildId, state.retentionDays, { silent: true });
        }

        if (removed > 0) {
          this.logger?.info?.(`FullLogs retention sweep (${trigger}) removed files: ${removed}`);
        }
      } catch (error) {
        this.logger?.warn?.(`FullLogs retention sweep error (${trigger}): ${error.message}`);
      }
    })();

    const trackedSweep = sweep.finally(() => {
      if (this.retentionSweepPromise === trackedSweep) {
        this.retentionSweepPromise = null;
      }
    });
    this.retentionSweepPromise = trackedSweep;
    return trackedSweep;
  }

  async pruneGuildFiles(guildId, retentionDays, { silent = true } = {}) {
    const normalized = normalizeRetentionDays(retentionDays, 30);
    const cutoff = Date.now() - (normalized * 24 * 60 * 60 * 1000);
    const guildDir = path.join(this.logsDir, String(guildId));
    const files = await fsp.readdir(guildDir).catch((error) => {
      if (error?.code === 'ENOENT') {
        return null;
      }
      throw error;
    });
    if (!files) return 0;

    let removed = 0;
    for (const fileName of files) {
      const match = /^(\d{4}-\d{2}-\d{2})\.jsonl$/.exec(fileName);
      if (!match) continue;
      const fileDate = Date.parse(`${match[1]}T00:00:00.000Z`);
      if (!Number.isFinite(fileDate) || fileDate >= cutoff) continue;
      const filePath = path.join(guildDir, fileName);
      await fsp.unlink(filePath).then(() => {
        removed += 1;
      }).catch((error) => {
        if (error?.code !== 'ENOENT') {
          throw error;
        }
      });
    }

    if (!silent && removed > 0) {
      this.logger?.info?.(`FullLogs removed ${removed} old files for guild ${guildId}.`);
    }
    return removed;
  }
}

module.exports = FullLogs;
module.exports.CATEGORY_META = CATEGORY_META;
module.exports.LOG_CATEGORY_KEYS = LOG_CATEGORY_KEYS;
