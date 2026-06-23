const path = require('path');
const { PermissionFlagsBits, Routes } = require('discord.js');
const { readOperatorConfigSync } = require('../core/config');

function normalizeContent(value) {
  return String(value || '')
    .toLowerCase()
    .replace(/\s+/g, ' ')
    .trim();
}

function escapeRegExp(value) {
  return String(value || '').replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function countMentionedUsers(content) {
  const matches = String(content || '').match(/<@!?(\d{15,22})>/g) || [];
  const unique = new Set(matches.map((entry) => {
    const match = entry.match(/\d{15,22}/);
    return match ? match[0] : entry;
  }));
  return unique.size;
}

class AutoMod {
  constructor(client, options = {}) {
    this.client = client;
    this.logger = options.logger || null;
    this.configPath = options.configPath || path.join(process.cwd(), 'config.json');
    this.config = this.loadConfig();
    this.messageHistory = new Map();
  }

  loadConfig() {
    const rootConfig = readOperatorConfigSync(this.configPath);
    this.config = rootConfig.automod || {};
    return this.config;
  }

  start() {
    this.loadConfig();
    this.pruneHistory();
  }

  getStatusSnapshot() {
    const rules = this.config.rules || {};
    const caps = rules.caps || {};
    const duplicate = rules.duplicate || {};
    const actions = this.config.actions || {};

    return {
      enabled: this.config.enabled === true,
      exemptUsers: Array.isArray(this.config.exemptUsers) ? this.config.exemptUsers.length : 0,
      exemptRoles: Array.isArray(this.config.exemptRoles) ? this.config.exemptRoles.length : 0,
      blockedWords: Array.isArray(rules.blockedWords) ? rules.blockedWords.length : 0,
      blockedRegex: Array.isArray(rules.blockedRegex) ? rules.blockedRegex.length : 0,
      blockInvites: rules.blockInvites === true,
      blockLinks: rules.blockLinks === true,
      allowedDomains: Array.isArray(rules.allowedDomains) ? rules.allowedDomains.length : 0,
      maxMentions: Number(rules.maxMentions || 0) || 0,
      capsEnabled: caps.enabled === true,
      capsMinLength: Number(caps.minLength || 0) || 0,
      capsRatio: Number(caps.ratio || 0) || 0,
      duplicateEnabled: duplicate.enabled === true,
      duplicateWindowSeconds: Number(duplicate.windowSeconds || 0) || 0,
      duplicateMaxRepeats: Number(duplicate.maxRepeats || 0) || 0,
      deleteMessage: actions.delete === true,
      timeoutMinutes: Number(actions.timeoutMinutes || 0) || 0,
      warnInChannel: actions.warnInChannel === true,
      logChannelId: String(this.config.logChannelId || '').trim() || null,
    };
  }

  getManagedRules() {
    const rules = this.config.rules || {};
    return {
      blockedWords: Array.isArray(rules.blockedWords) ? [...rules.blockedWords] : [],
      blockedRegex: Array.isArray(rules.blockedRegex) ? [...rules.blockedRegex] : [],
      allowedDomains: Array.isArray(rules.allowedDomains) ? [...rules.allowedDomains] : [],
    };
  }

  async handleMessage(message) {
    if (!this.config.enabled) return false;
    if (!message?.guild || !message.member || message.author?.bot) return false;
    if (this.isExempt(message)) return false;

    const violation = this.detectViolation(message);
    if (!violation) {
      return false;
    }

    await this.applyViolation(message, violation);
    return true;
  }

  isExempt(message) {
    if (message.member.permissions.has(PermissionFlagsBits.Administrator)) {
      return true;
    }

    const exemptUsers = Array.isArray(this.config.exemptUsers) ? this.config.exemptUsers.map(String) : [];
    if (exemptUsers.includes(String(message.author.id))) {
      return true;
    }

    const exemptRoles = Array.isArray(this.config.exemptRoles) ? this.config.exemptRoles.map(String) : [];
    for (const roleId of exemptRoles) {
      if (message.member.roles.cache.has(roleId)) {
        return true;
      }
    }

    return false;
  }

  detectViolation(message, options = {}) {
    const rules = options.rulesOverride || this.config.rules || {};
    const content = String(message.content || '');
    const normalized = normalizeContent(content);

    if (!content.trim()) {
      return null;
    }

    const blockedWords = Array.isArray(rules.blockedWords) ? rules.blockedWords : [];
    for (const word of blockedWords) {
      const trimmed = String(word || '').trim();
      if (!trimmed) continue;
      const regex = new RegExp(`(^|\\W)${escapeRegExp(trimmed.toLowerCase())}(\\W|$)`, 'i');
      if (regex.test(normalized)) {
        return {
          rule: 'blocked_word',
          details: `blocked word: ${trimmed}`,
        };
      }
    }

    const blockedRegex = Array.isArray(rules.blockedRegex) ? rules.blockedRegex : [];
    for (const rawPattern of blockedRegex) {
      try {
        const regex = new RegExp(String(rawPattern), 'i');
        if (regex.test(content)) {
          return {
            rule: 'blocked_regex',
            details: `blocked regex: ${rawPattern}`,
          };
        }
      } catch (error) {
        this.logger?.warn?.(`Automod invalid regex "${rawPattern}": ${error.message}`);
      }
    }

    if (rules.blockInvites === true && /(discord(?:app)?\.com\/invite|discord\.gg)\//i.test(content)) {
      return {
        rule: 'invite_link',
        details: 'Discord invite detected',
      };
    }

    if (rules.blockLinks === true) {
      const allowedDomains = Array.isArray(rules.allowedDomains)
        ? rules.allowedDomains.map((domain) => String(domain).toLowerCase())
        : [];
      const matches = content.match(/https?:\/\/[^\s<]+/gi) || [];
      for (const rawUrl of matches) {
        try {
          const url = new URL(rawUrl);
          const host = url.hostname.toLowerCase();
          const allowed = allowedDomains.some((domain) => host === domain || host.endsWith(`.${domain}`));
          if (!allowed) {
            return {
              rule: 'blocked_link',
              details: `link: ${host}`,
            };
          }
        } catch {}
      }
    }

    const maxMentions = Number(rules.maxMentions || 0) || 0;
    if (maxMentions > 0 && message.mentions.users.size > maxMentions) {
      return {
        rule: 'mention_spam',
        details: `mentions=${message.mentions.users.size}`,
      };
    }

    const caps = rules.caps || {};
    if (caps.enabled === true) {
      const letters = [...content].filter((char) => /[a-z]/i.test(char));
      const uppercase = letters.filter((char) => /[A-Z]/.test(char));
      const minLength = Number(caps.minLength || 0) || 0;
      const ratio = Number(caps.ratio || 0) || 0;
      if (letters.length >= minLength && letters.length > 0 && (uppercase.length / letters.length) >= ratio) {
        return {
          rule: 'caps_spam',
          details: `caps=${uppercase.length}/${letters.length}`,
        };
      }
    }

    const duplicate = rules.duplicate || {};
    if (options.skipDuplicate !== true && duplicate.enabled === true) {
      const duplicateViolation = this.trackDuplicateMessage(message, normalized, duplicate);
      if (duplicateViolation) {
        return duplicateViolation;
      }
    }

    return null;
  }

  trackDuplicateMessage(message, normalizedContent, duplicateConfig) {
    if (!normalizedContent) return null;

    const maxRepeats = Number(duplicateConfig.maxRepeats || 0) || 0;
    const windowSeconds = Number(duplicateConfig.windowSeconds || 0) || 0;
    if (maxRepeats <= 1 || windowSeconds <= 0) {
      return null;
    }

    const key = `${message.guild.id}:${message.author.id}`;
    const now = Date.now();
    const existing = this.messageHistory.get(key) || [];
    const windowMs = windowSeconds * 1000;
    const fresh = existing.filter((entry) => now - entry.ts <= windowMs);
    fresh.push({
      ts: now,
      content: normalizedContent,
    });
    this.messageHistory.set(key, fresh);

    const repeats = fresh.filter((entry) => entry.content === normalizedContent).length;
    if (repeats >= maxRepeats) {
      return {
        rule: 'duplicate_spam',
        details: `repeats=${repeats}/${maxRepeats}`,
      };
    }

    return null;
  }

  pruneHistory() {
    const duplicate = this.config.rules?.duplicate || {};
    const windowSeconds = Number(duplicate.windowSeconds || 0) || 0;
    if (windowSeconds <= 0) {
      this.messageHistory.clear();
      return;
    }

    const cutoff = Date.now() - (windowSeconds * 1000);
    for (const [key, entries] of this.messageHistory.entries()) {
      const fresh = entries.filter((entry) => entry.ts >= cutoff);
      if (fresh.length === 0) {
        this.messageHistory.delete(key);
        continue;
      }
      this.messageHistory.set(key, fresh);
    }
  }

  async applyViolation(message, violation) {
    const actions = this.config.actions || {};

    if (actions.delete === true) {
      await message.delete().catch((error) => {
        this.logger?.warn?.(`Automod delete failed (${message.guild.id}/${message.id}): ${error.message}`);
      });
    }

    const timeoutMinutes = Number(actions.timeoutMinutes || 0) || 0;
    if (timeoutMinutes > 0 && message.member.moderatable) {
      await message.member.timeout(timeoutMinutes * 60 * 1000, `AutoMod: ${violation.rule}`).catch((error) => {
        this.logger?.warn?.(`Automod timeout failed (${message.guild.id}/${message.author.id}): ${error.message}`);
      });
    }

    if (actions.warnInChannel === true && message.channel && 'send' in message.channel) {
      const warnText = String(actions.warnTemplate || 'Wiadomosc od {user} zostala usunieta przez AutoMod ({rule}).')
        .replaceAll('{user}', `<@${message.author.id}>`)
        .replaceAll('{rule}', violation.rule);
      const warning = await message.channel.send(warnText).catch(() => null);
      const deleteAfterSeconds = Number(actions.warnDeleteAfterSeconds || 0) || 0;
      if (warning && deleteAfterSeconds > 0) {
        setTimeout(() => {
          warning.delete().catch(() => {});
        }, deleteAfterSeconds * 1000).unref?.();
      }
    }

    await this.sendLog(message, violation);
  }

  async sendLog(message, violation) {
    const logChannelId = String(this.config.logChannelId || '').trim();
    if (!logChannelId) return;

    const channel = await this.client.channels.fetch(logChannelId).catch(() => null);
    if (!channel || !channel.isTextBased() || !('send' in channel)) {
      return;
    }

    const contentPreview = String(message.content || '').slice(0, 500) || '[no text]';
    const lines = [
      `AutoMod hit`,
      `Guild: ${message.guild.name} (${message.guild.id})`,
      `Channel: #${message.channel.name} (${message.channel.id})`,
      `User: ${message.author.tag} (${message.author.id})`,
      `Rule: ${violation.rule}`,
      `Details: ${violation.details}`,
      `Content: ${contentPreview}`,
    ];

    const payload = {
      flags: 32768, // IS_COMPONENTS_V2
      components: [
        {
          type: 17, // CONTAINER
          components: [
            {
              type: 10, // TEXT_DISPLAY
              content: '## AutoMod Hit',
            },
            {
              type: 10, // TEXT_DISPLAY
              content: lines.join('\n'),
            },
          ],
        },
      ],
    };

    await this.client.rest.post(Routes.channelMessages(logChannelId), {
      body: payload,
    }).catch((error) => {
      this.logger?.warn?.(`Automod log failed: ${error.message}`);
    });
  }

  testContent(content, options = {}) {
    const mode = String(options.mode || 'live').trim().toLowerCase();
    const candidateValue = String(options.value || '').trim();
    const syntheticMessage = {
      content: String(content || ''),
      guild: {
        id: 'automod-test-guild',
      },
      author: {
        id: 'automod-test-user',
      },
      mentions: {
        users: {
          size: countMentionedUsers(content),
        },
      },
    };

    const result = {
      mode,
      content: syntheticMessage.content,
      matched: false,
      violation: null,
      candidateType: null,
      candidateValue: candidateValue || null,
      note: this.config.rules?.duplicate?.enabled === true
        ? 'Duplicate rule nie jest symulowana w /automod test bez historii wiadomosci.'
        : null,
    };

    if (mode === 'word') {
      result.candidateType = 'word';
      result.violation = this.detectViolation(syntheticMessage, {
        rulesOverride: {
          blockedWords: candidateValue ? [candidateValue] : [],
        },
        skipDuplicate: true,
      });
    } else if (mode === 'regex') {
      result.candidateType = 'regex';
      result.violation = this.detectViolation(syntheticMessage, {
        rulesOverride: {
          blockedRegex: candidateValue ? [candidateValue] : [],
        },
        skipDuplicate: true,
      });
    } else {
      result.violation = this.detectViolation(syntheticMessage, {
        skipDuplicate: true,
      });
    }

    result.matched = Boolean(result.violation);
    return result;
  }
}

module.exports = AutoMod;
