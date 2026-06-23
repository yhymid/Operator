const path = require('path');
const { Collection, Routes } = require('discord.js');
const { CronJob } = require('cron');
const { readOperatorConfigSync } = require('../core/config');

class AutoClean {
  constructor(client, options = {}) {
    this.client = client;
    this.logger = options.logger;
    this.configPath = options.configPath || path.join(process.cwd(), 'config.json');
    this.stateStore = options.stateStore || null;

    this.config = this.loadConfig();
    this.isRunning = false;
    this.cronJob = null;
    this.stats = {
      runs: 0,
      totalDeleted: 0,
      lastRunAt: null,
    };
    this.warningTimer = null;
  }

  loadConfig() {
    this.config = readOperatorConfigSync(this.configPath);
    return this.config;
  }

  start() {
    this.loadConfig();

    if (this.cronJob) {
      this.cronJob.stop();
    }
    this.clearWarningTimer();

    if (this.config.enabled === false) {
      this.cronJob = null;
      this.updateNextRunForAllGuilds();
      this.logger?.info('AutoClean jest wylaczony.');
      return;
    }

    const schedule = this.getSchedule();
    const timezone = this.config.timezone || 'Europe/Warsaw';

    this.cronJob = new CronJob(
      schedule,
      async () => {
        await this.runCleanup('cron');
      },
      null,
      true,
      timezone,
    );

    this.updateNextRunForAllGuilds();
    this.scheduleNextWarning();
    this.logger?.info(`CRON ustawiony: ${schedule} (${timezone})`);
  }

  stop() {
    if (this.cronJob) {
      this.cronJob.stop();
    }
    this.clearWarningTimer();
  }

  getNextRunText() {
    if (!this.cronJob) return 'Brak harmonogramu';

    try {
      return this.cronJob.nextDate().toFormat('dd.MM.yyyy HH:mm:ss');
    } catch {
      return 'N/A';
    }
  }

  getNextRunIso() {
    if (!this.cronJob) return null;

    try {
      return this.cronJob.nextDate().toJSDate().toISOString();
    } catch {
      return null;
    }
  }

  async runCleanup(trigger = 'manual') {
    if (this.isRunning) {
      this.logger?.warn('Czyszczenie już trwa.');
      return null;
    }

    this.isRunning = true;
    try {
      this.loadConfig();

      if (this.config.enabled === false) {
        this.logger?.warn('AutoClean jest wylaczony.');
        return [];
      }

      const cleanupTargets = this.getPendingCleanupTargets();
      if (cleanupTargets.length === 0) {
        this.logger?.warn('Brak aktywnych kanałów do czyszczenia.');
        return [];
      }

      this.stats.runs += 1;
      this.stats.lastRunAt = new Date();

      const results = [];
      const consumedQueuedByGuild = new Map();

      for (const target of cleanupTargets) {
        const channelConfig = target.channelConfig || { id: target.channelId };
        const result = await this.cleanupChannel(channelConfig, trigger);
        results.push(result);

        if (target.consumeQueued === true && target.guildId) {
          if (!consumedQueuedByGuild.has(target.guildId)) {
            consumedQueuedByGuild.set(target.guildId, new Set());
          }
          consumedQueuedByGuild.get(target.guildId).add(target.channelId);
        }

        await this.sleep(1200);
      }

      for (const [guildId, channelIds] of consumedQueuedByGuild.entries()) {
        this.stateStore?.removeQueuedAutoCleanChannels?.(guildId, [...channelIds]);
      }

      const deletedNow = results.reduce((sum, item) => sum + item.deleted, 0);
      this.stats.totalDeleted += deletedNow;

      this.logger?.info(`Czyszczenie zakończone. Usunięto ${deletedNow} wiadomości.`);
      await this.sendCleanupLog(results, trigger);
      this.updateNextRunForAllGuilds();
      this.scheduleNextWarning();

      return results;
    } finally {
      this.isRunning = false;
    }
  }

  async runCleanupForChannel(channel, trigger = 'manual-channel') {
    if (this.isRunning) {
      this.logger?.warn('Czyszczenie już trwa.');
      return null;
    }

    this.isRunning = true;
    this.loadConfig();
    this.stats.runs += 1;
    this.stats.lastRunAt = new Date();

    try {
      const result = await this.cleanupResolvedChannel(channel, trigger);
      this.stats.totalDeleted += result.deleted;
      await this.sendCleanupLog([result], trigger);
      this.updateNextRunForAllGuilds();
      this.scheduleNextWarning();
      return result;
    } finally {
      this.isRunning = false;
    }
  }

  async cleanupChannel(channelConfig, trigger = 'manual') {
    try {
      const channel = await this.client.channels.fetch(channelConfig.id);
      if (!channel || !channel.isTextBased()) {
        throw new Error('Kanał nie istnieje lub nie jest tekstowy');
      }
      return this.cleanupResolvedChannel(channel, trigger, channelConfig);
    } catch (error) {
      this.logger?.error(`Błąd kanału ${channelConfig.id}: ${error.message}`);
      return {
        channelId: channelConfig.id,
        channelName: 'unknown',
        guildId: null,
        deleted: 0,
        error: error.message,
        durationMs: 0,
        trigger,
      };
    }
  }

  async cleanupResolvedChannel(channel, trigger = 'manual', providedChannelConfig = null) {
    const startedAt = Date.now();
    const settings = this.getEffectiveChannelSettings(channel.id, providedChannelConfig);
    const result = {
      channelId: channel.id,
      channelName: channel.name || 'unknown',
      guildId: channel.guild?.id || null,
      deleted: 0,
      error: null,
      durationMs: 0,
      trigger,
      settings,
    };

    try {
      const candidates = await this.fetchMessagesToDelete(channel, settings);
      result.deleted = await this.deleteMessages(channel, candidates, settings);
      await this.sendAutoInfoMessage(channel);
    } catch (error) {
      result.error = error.message;
      this.logger?.error(`Błąd kanału ${channel.id}: ${error.message}`);
    }

    result.durationMs = Date.now() - startedAt;
    if (result.guildId && this.stateStore?.markCleanupResult) {
      this.stateStore.markCleanupResult(result.guildId, {
        trigger: result.trigger,
        deleted: result.deleted,
        error: result.error,
        durationMs: result.durationMs,
      });
    }
    return result;
  }

  updateNextRunForAllGuilds() {
    if (!this.stateStore?.setNextRunAt) return;

    const nextRunAt = this.getNextRunIso();

    for (const guild of this.client.guilds.cache.values()) {
      this.stateStore.setNextRunAt(guild.id, nextRunAt);
    }
  }

  channelBelongsToGuild(channelId, guildId, channelConfig = null) {
    if (!guildId) return true;

    const configuredGuildId = channelConfig?.guildId ? String(channelConfig.guildId) : null;
    if (configuredGuildId) {
      return configuredGuildId === String(guildId);
    }

    const cachedChannel = this.client.channels.cache.get(String(channelId));
    const cachedGuildId = cachedChannel?.guild?.id || cachedChannel?.guildId || null;
    if (cachedGuildId) {
      return String(cachedGuildId) === String(guildId);
    }

    return this.client.guilds.cache.size === 1;
  }

  getConfiguredChannelEntries(guildId = null) {
    const enabledChannels = (this.config.channels || []).filter((channel) => channel.enabled !== false);
    return enabledChannels
      .filter((channelConfig) => this.channelBelongsToGuild(channelConfig.id, guildId, channelConfig))
      .map((channelConfig) => ({
        channelId: String(channelConfig.id),
        guildId: guildId ? String(guildId) : this.resolveGuildId(channelConfig.id, channelConfig),
        channelConfig,
      }));
  }

  getQueuedChannelEntries(guildId = null) {
    if (!this.stateStore?.listQueuedAutoCleanChannels) {
      return [];
    }

    if (guildId) {
      return this.stateStore.listQueuedAutoCleanChannels(guildId).map((entry) => ({
        guildId: String(guildId),
        channelId: String(entry?.channelId || ''),
        queuedAt: entry?.queuedAt || null,
        queuedBy: entry?.queuedBy ? String(entry.queuedBy) : null,
      })).filter((entry) => entry.channelId);
    }

    const entries = [];
    for (const guild of this.client.guilds.cache.values()) {
      const queued = this.stateStore.listQueuedAutoCleanChannels(guild.id);
      for (const entry of queued) {
        if (!entry?.channelId) continue;
        entries.push({
          guildId: String(guild.id),
          channelId: String(entry.channelId),
          queuedAt: entry.queuedAt || null,
          queuedBy: entry.queuedBy ? String(entry.queuedBy) : null,
        });
      }
    }
    return entries;
  }

  resolveGuildId(channelId, channelConfig = null) {
    const configuredGuildId = channelConfig?.guildId ? String(channelConfig.guildId) : null;
    if (configuredGuildId) {
      return configuredGuildId;
    }

    const cachedChannel = this.client.channels.cache.get(String(channelId));
    const cachedGuildId = cachedChannel?.guild?.id || cachedChannel?.guildId || null;
    if (cachedGuildId) {
      return String(cachedGuildId);
    }

    if (this.client.guilds.cache.size === 1) {
      return this.client.guilds.cache.first()?.id || null;
    }

    return null;
  }

  getPendingCleanupTargets(guildId = null) {
    const targets = new Map();

    for (const entry of this.getConfiguredChannelEntries(guildId)) {
      targets.set(entry.channelId, {
        channelId: entry.channelId,
        guildId: entry.guildId || this.resolveGuildId(entry.channelId, entry.channelConfig),
        channelConfig: entry.channelConfig,
        source: 'config',
        consumeQueued: false,
        queuedAt: null,
        queuedBy: null,
      });
    }

    for (const entry of this.getQueuedChannelEntries(guildId)) {
      const existing = targets.get(entry.channelId);
      if (existing) {
        existing.consumeQueued = true;
        existing.queuedAt = entry.queuedAt;
        existing.queuedBy = entry.queuedBy;
        existing.source = 'config+queued';
        continue;
      }

      targets.set(entry.channelId, {
        channelId: entry.channelId,
        guildId: entry.guildId || this.resolveGuildId(entry.channelId),
        channelConfig: null,
        source: 'queued',
        consumeQueued: true,
        queuedAt: entry.queuedAt,
        queuedBy: entry.queuedBy,
      });
    }

    return [...targets.values()];
  }

  queueChannelForNextCleanup(channel, queuedBy = null) {
    if (!channel?.guildId || !this.stateStore?.addQueuedAutoCleanChannel) {
      throw new Error('State store AutoClean nie jest dostepny.');
    }

    const existing = this.getQueuedChannelEntries(channel.guildId)
      .find((entry) => entry.channelId === String(channel.id));
    if (existing) {
      return {
        queued: false,
        alreadyQueued: true,
        entry: existing,
      };
    }

    this.stateStore.addQueuedAutoCleanChannel(channel.guildId, channel.id, {
      queuedBy,
      queuedAt: new Date().toISOString(),
    });

    const entry = this.getQueuedChannelEntries(channel.guildId)
      .find((item) => item.channelId === String(channel.id)) || null;

    return {
      queued: true,
      alreadyQueued: false,
      entry,
    };
  }

  resetQueuedChannel(channel) {
    if (!channel?.guildId || !this.stateStore?.removeQueuedAutoCleanChannel) {
      throw new Error('State store AutoClean nie jest dostepny.');
    }

    const existing = this.getQueuedChannelEntries(channel.guildId)
      .find((entry) => entry.channelId === String(channel.id));
    if (!existing) {
      return {
        removed: false,
        existed: false,
      };
    }

    this.stateStore.removeQueuedAutoCleanChannel(channel.guildId, channel.id);
    return {
      removed: true,
      existed: true,
      entry: existing,
    };
  }

  async fetchMessagesToDelete(channel, settings) {
    const messages = [];
    let before;
    const maxDelete = Number(settings?.deleteLimit || 0);
    const maxAgeDays = Number(settings?.deleteMessagesOlderThanDays || 0);
    const minCreatedTs = maxAgeDays > 0
      ? Date.now() - (maxAgeDays * 24 * 60 * 60 * 1000)
      : null;

    while (true) {
      const batch = await channel.messages.fetch({
        limit: 100,
        ...(before ? { before } : {}),
      });

      if (batch.size === 0) break;

      for (const [, msg] of batch) {
        if (!this.shouldDeleteMessage(msg, settings, minCreatedTs)) {
          continue;
        }

        messages.push(msg);
        if (maxDelete > 0 && messages.length >= maxDelete) {
          return messages;
        }
      }

      before = batch.last()?.id;
      if (!before || batch.size < 100) break;

      await this.sleep(500);
    }

    return messages;
  }

  async deleteMessages(channel, messages, settings) {
    if (!Array.isArray(messages) || messages.length === 0) {
      return 0;
    }

    if (this.config.dryRun) {
      this.logger?.warn(`[DRY RUN] Pominieto usuwanie ${messages.length} wiadomosci z #${channel.name}`);
      return 0;
    }

    const now = Date.now();
    const twoWeeksMs = 14 * 24 * 60 * 60 * 1000;

    const fresh = messages.filter((msg) => now - msg.createdTimestamp < twoWeeksMs);
    const old = messages.filter((msg) => now - msg.createdTimestamp >= twoWeeksMs);

    let deleted = 0;

    const freshChunks = this.chunk(fresh, 100);
    for (const chunk of freshChunks) {
      const collection = new Collection(chunk.map((msg) => [msg.id, msg]));
      try {
        const bulk = await channel.bulkDelete(collection, true);
        deleted += bulk.size;
      } catch {
        for (const msg of chunk) {
          try {
            await msg.delete();
            deleted += 1;
            await this.sleep(700);
          } catch {}
        }
      }

      await this.sleep(900);
    }

    for (const msg of old) {
      try {
        await msg.delete();
        deleted += 1;
        await this.sleep(900);
      } catch {}
    }

    return deleted;
  }

  getChannelConfig(channelId) {
    const channels = Array.isArray(this.config.channels) ? this.config.channels : [];
    return channels.find((channel) => String(channel.id) === String(channelId)) || null;
  }

  getEffectiveChannelSettings(channelId, providedChannelConfig = null) {
    const channelConfig = providedChannelConfig || this.getChannelConfig(channelId) || {};
    const whitelist = this.config.whitelist || {};
    const notifyBeforeDelete = this.config.notifyBeforeDelete || {};

    return {
      channelId: String(channelId),
      enabled: channelConfig.enabled !== false,
      deleteLimit: Number(channelConfig.deleteLimit ?? this.config.deleteLimit ?? 0) || 0,
      deletePinned: channelConfig.deletePinned === true || this.config.deletePinned === true,
      deleteMessagesOlderThanDays: Number(
        channelConfig.deleteMessagesOlderThanDays
        ?? this.config.deleteMessagesOlderThanDays
        ?? 0,
      ) || 0,
      whitelistUsers: Array.isArray(whitelist.users) ? whitelist.users.map(String) : [],
      whitelistRoles: Array.isArray(whitelist.roles) ? whitelist.roles.map(String) : [],
      notifyBeforeDeleteEnabled: notifyBeforeDelete.enabled === true,
      notifyBeforeDeleteMinutes: Number(notifyBeforeDelete.minutesBefore || 0) || 0,
    };
  }

  getStatusSnapshot(guildId = null) {
    this.loadConfig();

    const enabledChannels = this.getConfiguredChannelEntries(guildId);
    const queuedChannels = this.getQueuedChannelEntries(guildId);
    const notify = this.config.notifyBeforeDelete || {};
    const persistedState = guildId && this.stateStore?.getAutoCleanState
      ? this.stateStore.getAutoCleanState(guildId)
      : null;

    return {
      enabled: this.config.enabled !== false,
      enabledChannels: enabledChannels.length,
      permanentChannels: enabledChannels.map((entry) => ({
        channelId: entry.channelId,
        deleteLimit: Number(entry.channelConfig?.deleteLimit ?? this.config.deleteLimit ?? 0) || 0,
        deletePinned: entry.channelConfig?.deletePinned === true || this.config.deletePinned === true,
      })),
      queuedChannels,
      queuedChannelsCount: queuedChannels.length,
      schedule: this.getSchedule(),
      timezone: this.config.timezone || 'Europe/Warsaw',
      nextRunText: this.getNextRunText(),
      nextRunAt: persistedState?.nextRunAt || this.getNextRunIso(),
      dryRun: this.config.dryRun === true,
      deleteIntervalText: this.getIntervalText(),
      deleteMessagesOlderThanDays: Number(this.config.deleteMessagesOlderThanDays || 0) || 0,
      deleteLimit: Number(this.config.deleteLimit || 0) || 0,
      deletePinned: this.config.deletePinned === true,
      whitelistUsers: Array.isArray(this.config.whitelist?.users) ? this.config.whitelist.users.length : 0,
      whitelistRoles: Array.isArray(this.config.whitelist?.roles) ? this.config.whitelist.roles.length : 0,
      notifyBeforeDelete: notify.enabled === true
        ? `${Number(notify.minutesBefore || 0) || 0} min`
        : 'off',
      logChannelId: this.config.logChannelId || null,
      runs: this.stats.runs,
      totalDeleted: this.stats.totalDeleted,
      lastRunAt: persistedState?.lastRunAt || (this.stats.lastRunAt ? this.stats.lastRunAt.toISOString() : null),
      lastDeleted: persistedState?.lastDeleted ?? 0,
      lastError: persistedState?.lastError || null,
      lastTrigger: persistedState?.lastTrigger || null,
      lastDurationMs: persistedState?.lastDurationMs ?? null,
    };
  }

  shouldDeleteMessage(message, settings, minCreatedTs = null) {
    if (!message) return false;
    if (!settings?.deletePinned && message.pinned) return false;

    const userId = String(message.author?.id || '');
    if (userId && settings.whitelistUsers.includes(userId)) {
      return false;
    }

    const memberRoles = message.member?.roles?.cache;
    if (memberRoles && settings.whitelistRoles.length > 0) {
      for (const roleId of settings.whitelistRoles) {
        if (memberRoles.has(roleId)) {
          return false;
        }
      }
    }

    if (minCreatedTs && message.createdTimestamp > minCreatedTs) {
      return false;
    }

    return true;
  }

  clearWarningTimer() {
    if (!this.warningTimer) return;
    clearTimeout(this.warningTimer);
    this.warningTimer = null;
  }

  scheduleNextWarning() {
    this.clearWarningTimer();
    if (!this.cronJob) return;

    const notify = this.config.notifyBeforeDelete || {};
    const enabled = notify.enabled === true;
    const minutesBefore = Number(notify.minutesBefore || 0) || 0;
    if (!enabled || minutesBefore <= 0) {
      return;
    }

    let nextRunDate = null;
    try {
      nextRunDate = this.cronJob.nextDate().toJSDate();
    } catch {
      return;
    }

    const fireAtMs = nextRunDate.getTime() - (minutesBefore * 60 * 1000);
    const delayMs = fireAtMs - Date.now();
    if (delayMs <= 0) {
      return;
    }

    this.warningTimer = setTimeout(() => {
      this.sendPreDeleteWarnings().catch((error) => {
        this.logger?.error(`Blad notifyBeforeDelete: ${error.message}`);
      }).finally(() => {
        this.scheduleNextWarning();
      });
    }, delayMs);
    this.warningTimer.unref?.();
  }

  async sendPreDeleteWarnings() {
    const notify = this.config.notifyBeforeDelete || {};
    if (notify.enabled !== true) return;

    const pendingChannels = this.getPendingCleanupTargets();
    if (pendingChannels.length === 0) return;

    const minutes = Number(notify.minutesBefore || 0) || 0;
    const template = String(notify.message || 'Za {minutes} minut kanal zostanie wyczyszczony.');
    const text = template.replaceAll('{minutes}', String(minutes));

    for (const target of pendingChannels) {
      const channel = await this.client.channels.fetch(target.channelId).catch(() => null);
      if (!channel || !channel.isTextBased() || !('send' in channel)) {
        continue;
      }
      await channel.send(text).catch((error) => {
        this.logger?.warn(`Nie udalo sie wyslac notifyBeforeDelete na #${channel?.name || target.channelId}: ${error.message}`);
      });
      await this.sleep(300);
    }
  }

  async sendCleanupLog(results, trigger) {
    const logChannelId = String(this.config.logChannelId || '').trim();
    if (!logChannelId) {
      return;
    }

    const logChannel = await this.client.channels.fetch(logChannelId).catch(() => null);
    if (!logChannel || !logChannel.isTextBased() || !('send' in logChannel)) {
      return;
    }

    const deleted = results.reduce((sum, item) => sum + Number(item.deleted || 0), 0);
    const failed = results.filter((item) => item.error).length;
    const lines = [
      `AutoClean report`,
      `Trigger: ${trigger}`,
      `Deleted: ${deleted}`,
      `Errors: ${failed}`,
      `Channels: ${results.length}`,
    ];

    for (const item of results.slice(0, 10)) {
      const suffix = item.error ? ` error=${item.error}` : '';
      lines.push(`- #${item.channelName} (${item.channelId}) deleted=${item.deleted}${suffix}`);
    }

    const payload = {
      flags: 32768, // IS_COMPONENTS_V2
      components: [
        {
          type: 17, // CONTAINER
          components: [
            {
              type: 10, // TEXT_DISPLAY
              content: '## AutoClean Report',
            },
            {
              type: 10, // TEXT_DISPLAY
              content: lines.join('\n'),
            },
          ],
        },
      ],
    };

    await this.client.rest.post(Routes.channelMessages(logChannel.id), {
      body: payload,
    }).catch((error) => {
      this.logger?.warn(`Nie udalo sie wyslac logu AutoClean: ${error.message}`);
    });
  }

  async sendAutoInfoMessage(channel) {
    const intervalText = this.getIntervalText();
    const text = `This channel automatically clears messages every ${intervalText}.`;
    const payload = {
      flags: 32768, // IS_COMPONENTS_V2
      components: [
        {
          type: 17, // CONTAINER
          components: [
            {
              type: 10, // TEXT_DISPLAY
              content: text,
            },
          ],
        },
      ],
    };

    try {
      await this.client.rest.post(Routes.channelMessages(channel.id), {
        body: payload,
      });
    } catch (error) {
      const code = error?.code ? ` code=${error.code}` : '';
      const status = error?.status ? ` status=${error.status}` : '';
      const raw = error?.rawError ? ` raw=${JSON.stringify(error.rawError)}` : '';
      const sent = ` payload=${JSON.stringify(payload)}`;
      this.logger?.error(
        `Nie udalo sie wyslac Components v2 na #${channel.name}:${status}${code} ${error.message}${raw}${sent}`,
      );
    }
  }

  getIntervalText() {
    if (this.config.deleteIntervalMinutes && this.config.deleteIntervalMinutes > 0) {
      return this.formatEnglishUnit(this.config.deleteIntervalMinutes, 'minute');
    }

    if (this.config.deleteIntervalHours && this.config.deleteIntervalHours > 0) {
      return this.formatEnglishUnit(this.config.deleteIntervalHours, 'hour');
    }

    if (this.config.deleteIntervalDays && this.config.deleteIntervalDays > 0) {
      return this.formatEnglishUnit(this.config.deleteIntervalDays, 'day');
    }

    return 'configured interval';
  }

  getSchedule() {
    if (this.config.cronSchedule && this.config.cronSchedule.trim().length > 0) {
      return this.config.cronSchedule;
    }

    if (this.config.deleteIntervalMinutes && this.config.deleteIntervalMinutes > 0) {
      return `*/${this.config.deleteIntervalMinutes} * * * *`;
    }

    if (this.config.deleteIntervalHours && this.config.deleteIntervalHours > 0) {
      return `0 */${this.config.deleteIntervalHours} * * *`;
    }

    if (this.config.deleteIntervalDays && this.config.deleteIntervalDays > 0) {
      return `0 0 */${this.config.deleteIntervalDays} * *`;
    }

    return '0 3 * * 0';
  }

  formatEnglishUnit(value, unit) {
    const normalized = Number(value) || 0;
    if (normalized === 1) {
      return `1 ${unit}`;
    }
    return `${normalized} ${unit}s`;
  }

  chunk(array, size) {
    const chunks = [];
    for (let i = 0; i < array.length; i += size) {
      chunks.push(array.slice(i, i + size));
    }
    return chunks;
  }

  sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }
}

module.exports = AutoClean;
