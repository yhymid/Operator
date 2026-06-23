const fs = require('fs');
const path = require('path');

let store = null;
let guildStateCache = new Map();
const REPLACE_VALUE = Symbol('replaceStateValue');

function initServerStateStore({ cacheDir, logger }) {
  if (!cacheDir) {
    throw new Error('cacheDir is required');
  }

  fs.mkdirSync(cacheDir, { recursive: true });
  guildStateCache = new Map();
  store = {
    cacheDir,
    logger: logger || null,
  };

  return {
    loadGuildState,
    saveGuildState,
    markGuildOnline,
    markGuildOffline,
    getAutoCleanState,
    saveAutoCleanState,
    markCleanupResult,
    setNextRunAt,
    listQueuedAutoCleanChannels,
    addQueuedAutoCleanChannel,
    removeQueuedAutoCleanChannel,
    removeQueuedAutoCleanChannels,
    migrateLegacyServerCache,
    getVerificationState,
    saveVerificationState,
    setVerificationPanel,
    addVerificationException,
    removeVerificationException,
    listVerificationExceptions,
    setUserVerificationExpiry,
    removeUserVerificationExpiry,
    getExpiredVerificationUsers,
    getVerificationChallenge,
    saveVerificationChallenge,
    removeVerificationChallenge,
    pruneExpiredVerificationChallenges,
    getVerificationLockout,
    setVerificationLockout,
    removeVerificationLockout,
    pruneExpiredVerificationLockouts,
    getAnnouncementsState,
    saveAnnouncementsState,
    getLogsState,
    saveLogsState,
  };
}

function ensureStore() {
  if (!store) {
    throw new Error('State store is not initialized');
  }
}

function getGuildFilePath(guildId) {
  ensureStore();
  return path.join(store.cacheDir, `${guildId}.json`);
}

function nowIso() {
  return new Date().toISOString();
}

function createDefaultState(guildId, guildName = null) {
  return {
    version: 1,
    guildId: String(guildId),
    guildName,
    presence: {
      status: 'offline',
      reason: null,
      lastSeenAt: null,
      lastOnlineAt: null,
      lastOfflineAt: null,
    },
    runtime: {
      bootCount: 0,
      lastBootAt: null,
      lastExitReason: null,
    },
    autoclean: {
      nextRunAt: null,
      lastRunAt: null,
      lastTrigger: null,
      lastDeleted: 0,
      lastError: null,
      lastDurationMs: null,
      queuedChannels: [],
    },
    verification: {
      enabled: false,
      roleId: null,
      panel: {
        channelId: null,
        messageId: null,
        customId: null,
      },
      exceptions: [],
      expirations: {},
      challenges: {},
      lockouts: {},
      lastSweepAt: null,
      lastFullSweepAt: null,
    },
    announcements: {
      webhooks: {},
      profiles: {},
      items: [],
      lastSweepAt: null,
    },
    logs: {
      enabled: false,
      channelId: null,
      retentionDays: 30,
      forwardMode: 'all',
      includeBots: false,
      disabledCategories: [],
    },
  };
}

function deepMergeState(base, patch) {
  const next = { ...base };

  for (const [key, value] of Object.entries(patch || {})) {
    if (isReplaceValue(value)) {
      next[key] = clonePlainObject(value.value);
      continue;
    }

    if (
      value &&
      typeof value === 'object' &&
      !Array.isArray(value) &&
      typeof next[key] === 'object' &&
      next[key] !== null &&
      !Array.isArray(next[key])
    ) {
      next[key] = deepMergeState(next[key], value);
      continue;
    }

    next[key] = value;
  }

  return next;
}

function hasOwn(obj, key) {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function clonePlainObject(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return value;
  }
  return { ...value };
}

function cloneStateValue(value) {
  if (typeof globalThis.structuredClone === 'function') {
    return globalThis.structuredClone(value);
  }
  return JSON.parse(JSON.stringify(value));
}

function sanitizeAnnouncementItems(items) {
  if (!Array.isArray(items)) return items;
  return items.map((item) => {
    if (!item || typeof item !== 'object') {
      return item;
    }

    const next = { ...item };
    delete next.webhookToken;
    return next;
  });
}

function replaceStateValue(value) {
  return {
    [REPLACE_VALUE]: true,
    value,
  };
}

function isReplaceValue(value) {
  return Boolean(value && typeof value === 'object' && value[REPLACE_VALUE] === true);
}

function readStateFile(filePath) {
  if (!fs.existsSync(filePath)) return null;

  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch (error) {
    store?.logger?.error(`Nie mozna odczytac cache ${filePath}: ${error.message}`);
    return null;
  }
}

function writeStateFileAtomic(filePath, payload) {
  const dirPath = path.dirname(filePath);
  const tempPath = path.join(
    dirPath,
    `.${path.basename(filePath)}.${process.pid}.${Date.now()}.tmp`,
  );
  const json = `${JSON.stringify(payload, null, 2)}\n`;

  fs.mkdirSync(dirPath, { recursive: true });

  try {
    fs.writeFileSync(tempPath, json, 'utf8');
    const fd = fs.openSync(tempPath, 'r');
    try {
      fs.fsyncSync(fd);
    } finally {
      fs.closeSync(fd);
    }
    fs.renameSync(tempPath, filePath);
  } catch (error) {
    try {
      if (fs.existsSync(tempPath)) {
        fs.unlinkSync(tempPath);
      }
    } catch {}
    throw error;
  }
}

function getCachedGuildState(guildId) {
  const normalizedGuildId = String(guildId);
  if (guildStateCache.has(normalizedGuildId)) {
    return guildStateCache.get(normalizedGuildId);
  }

  const filePath = getGuildFilePath(normalizedGuildId);
  const current = readStateFile(filePath);
  const next = current
    ? deepMergeState(createDefaultState(normalizedGuildId), current)
    : createDefaultState(normalizedGuildId);

  guildStateCache.set(normalizedGuildId, next);
  return next;
}

function loadGuildState(guildId) {
  return cloneStateValue(getCachedGuildState(guildId));
}

function saveGuildState(guildId, updaterOrPartial) {
  const normalizedGuildId = String(guildId);
  const current = getCachedGuildState(normalizedGuildId);
  const patch = typeof updaterOrPartial === 'function'
    ? updaterOrPartial(cloneStateValue(current))
    : updaterOrPartial;
  const next = deepMergeState(current, patch || {});

  if (!next.guildId) {
    next.guildId = normalizedGuildId;
  }

  guildStateCache.set(normalizedGuildId, next);
  writeStateFileAtomic(getGuildFilePath(normalizedGuildId), next);
  return cloneStateValue(next);
}

function markGuildOnline(guild, reason = 'startup') {
  const timestamp = nowIso();

  return saveGuildState(guild.id, (current) => ({
    guildId: String(guild.id),
    guildName: guild.name || current.guildName || null,
    presence: {
      status: 'online',
      reason,
      lastSeenAt: timestamp,
      lastOnlineAt: timestamp,
    },
    runtime: {
      bootCount: (current.runtime?.bootCount || 0) + 1,
      lastBootAt: timestamp,
    },
  }));
}

function markGuildOffline(guild, reason = 'shutdown') {
  const timestamp = nowIso();
  const guildId = guild.id || guild.guildId;
  const guildName = guild.name || guild.guildName || null;

  return saveGuildState(guildId, (current) => ({
    guildId: String(guildId),
    guildName: guildName || current.guildName || null,
    presence: {
      status: 'offline',
      reason,
      lastSeenAt: timestamp,
      lastOfflineAt: timestamp,
    },
    runtime: {
      lastExitReason: reason,
    },
  }));
}

function markCleanupResult(guildId, payload) {
  const timestamp = nowIso();

  return saveGuildState(guildId, {
    autoclean: {
      lastRunAt: timestamp,
      lastTrigger: payload.trigger || 'unknown',
      lastDeleted: Number(payload.deleted || 0),
      lastError: payload.error || null,
      lastDurationMs: Number(payload.durationMs || 0),
    },
  });
}

function getAutoCleanState(guildId) {
  const state = loadGuildState(guildId);
  return deepMergeState({
    nextRunAt: null,
    lastRunAt: null,
    lastTrigger: null,
    lastDeleted: 0,
    lastError: null,
    lastDurationMs: null,
    queuedChannels: [],
  }, state.autoclean || {});
}

function saveAutoCleanState(guildId, patchOrUpdater) {
  return saveGuildState(guildId, (current) => {
    const currentAutoClean = current.autoclean || getAutoCleanState(guildId);
    const patch = typeof patchOrUpdater === 'function'
      ? patchOrUpdater(currentAutoClean, current)
      : patchOrUpdater;

    return {
      autoclean: patch || {},
    };
  });
}

function setNextRunAt(guildId, isoDateString) {
  return saveAutoCleanState(guildId, {
    nextRunAt: isoDateString || null,
  });
}

function listQueuedAutoCleanChannels(guildId) {
  const autoclean = getAutoCleanState(guildId);
  return Array.isArray(autoclean.queuedChannels) ? autoclean.queuedChannels : [];
}

function addQueuedAutoCleanChannel(guildId, channelId, metadata = {}) {
  const normalizedChannelId = String(channelId);
  const queuedAt = metadata.queuedAt || nowIso();
  const queuedBy = metadata.queuedBy ? String(metadata.queuedBy) : null;

  return saveAutoCleanState(guildId, (autoclean) => {
    const queuedChannels = Array.isArray(autoclean.queuedChannels)
      ? [...autoclean.queuedChannels]
      : [];

    const existingIndex = queuedChannels.findIndex((entry) => String(entry?.channelId || '') === normalizedChannelId);
    if (existingIndex >= 0) {
      const currentEntry = queuedChannels[existingIndex] || {};
      queuedChannels[existingIndex] = {
        channelId: normalizedChannelId,
        queuedAt: currentEntry.queuedAt || queuedAt,
        queuedBy: currentEntry.queuedBy || queuedBy,
      };
      return { queuedChannels };
    }

    queuedChannels.push({
      channelId: normalizedChannelId,
      queuedAt,
      queuedBy,
    });

    return { queuedChannels };
  });
}

function removeQueuedAutoCleanChannel(guildId, channelId) {
  const normalizedChannelId = String(channelId);
  return saveAutoCleanState(guildId, (autoclean) => ({
    queuedChannels: (Array.isArray(autoclean.queuedChannels) ? autoclean.queuedChannels : [])
      .filter((entry) => String(entry?.channelId || '') !== normalizedChannelId),
  }));
}

function removeQueuedAutoCleanChannels(guildId, channelIds) {
  const normalized = new Set((Array.isArray(channelIds) ? channelIds : []).map((item) => String(item)));
  return saveAutoCleanState(guildId, (autoclean) => ({
    queuedChannels: (Array.isArray(autoclean.queuedChannels) ? autoclean.queuedChannels : [])
      .filter((entry) => !normalized.has(String(entry?.channelId || ''))),
  }));
}

function migrateLegacyServerCache() {
  ensureStore();

  const files = fs.readdirSync(store.cacheDir).filter((file) => file.endsWith('.json'));
  let migrated = 0;
  let removed = 0;

  for (const fileName of files) {
    const match = /^(.+)-(\d+)\.json$/.exec(fileName);
    if (!match) continue;

    const guildId = match[2];
    const legacyPath = path.join(store.cacheDir, fileName);
    const targetPath = getGuildFilePath(guildId);

    if (legacyPath === targetPath) continue;

    if (fs.existsSync(targetPath)) {
      fs.unlinkSync(legacyPath);
      removed += 1;
      continue;
    }

    fs.renameSync(legacyPath, targetPath);
    migrated += 1;
  }

  if (migrated > 0 || removed > 0) {
    store.logger?.info(`Migracja cache legacy: przeniesiono=${migrated}, usunieto=${removed}`);
  }
  return { migrated, removed };
}

function getAnnouncementsState(guildId) {
  const state = loadGuildState(guildId);
  return deepMergeState({
    webhooks: {},
    profiles: {},
    items: [],
    lastSweepAt: null,
  }, state.announcements || {});
}

function saveAnnouncementsState(guildId, partial) {
  const patch = partial && typeof partial === 'object' && !Array.isArray(partial)
    ? { ...partial }
    : partial;

  if (patch && typeof patch === 'object' && !Array.isArray(patch)) {
    if (hasOwn(patch, 'webhooks')) {
      patch.webhooks = replaceStateValue(clonePlainObject(patch.webhooks));
    }
    if (hasOwn(patch, 'profiles')) {
      patch.profiles = replaceStateValue(clonePlainObject(patch.profiles));
    }
    if (hasOwn(patch, 'items')) {
      patch.items = sanitizeAnnouncementItems(patch.items);
    }
  }

  return saveGuildState(guildId, {
    announcements: patch || {},
  });
}

function getVerificationState(guildId) {
  const state = loadGuildState(guildId);
  return state.verification || createDefaultState(guildId).verification;
}

function saveVerificationState(guildId, patchOrUpdater) {
  return saveGuildState(guildId, (current) => {
    const currentVerification = current.verification || createDefaultState(guildId).verification;
    const patch = typeof patchOrUpdater === 'function'
      ? patchOrUpdater(currentVerification, current)
      : patchOrUpdater;
    const patchForMerge = patch && typeof patch === 'object' && !Array.isArray(patch) ? { ...patch } : patch;

    // These maps are often updated by deleting keys. Deep-merge keeps removed keys,
    // so when they are present in patch we mark them as full replacement payloads.
    if (patchForMerge && typeof patchForMerge === 'object' && !Array.isArray(patchForMerge)) {
      if (hasOwn(patchForMerge, 'expirations')) {
        patchForMerge.expirations = replaceStateValue(clonePlainObject(patchForMerge.expirations));
      }
      if (hasOwn(patchForMerge, 'challenges')) {
        patchForMerge.challenges = replaceStateValue(clonePlainObject(patchForMerge.challenges));
      }
      if (hasOwn(patchForMerge, 'lockouts')) {
        patchForMerge.lockouts = replaceStateValue(clonePlainObject(patchForMerge.lockouts));
      }
    }

    return {
      verification: patchForMerge || {},
    };
  });
}

function setVerificationPanel(guildId, { channelId, messageId, customId, roleId }) {
  return saveVerificationState(guildId, {
    enabled: true,
    roleId: String(roleId),
    panel: {
      channelId: String(channelId),
      messageId: String(messageId),
      customId: String(customId),
    },
  });
}

function addVerificationException(guildId, userId) {
  return saveVerificationState(guildId, (verification) => {
    const existing = Array.isArray(verification.exceptions) ? verification.exceptions : [];
    const normalized = String(userId);

    if (existing.includes(normalized)) {
      return { exceptions: existing };
    }

    return { exceptions: [...existing, normalized] };
  });
}

function removeVerificationException(guildId, userId) {
  return saveVerificationState(guildId, (verification) => {
    const existing = Array.isArray(verification.exceptions) ? verification.exceptions : [];
    const normalized = String(userId);
    return { exceptions: existing.filter((id) => id !== normalized) };
  });
}

function listVerificationExceptions(guildId) {
  const verification = getVerificationState(guildId);
  return Array.isArray(verification.exceptions) ? verification.exceptions : [];
}

function setUserVerificationExpiry(guildId, userId, isoDate) {
  return saveVerificationState(guildId, (verification) => ({
    expirations: {
      ...(verification.expirations || {}),
      [String(userId)]: isoDate,
    },
  }));
}

function removeUserVerificationExpiry(guildId, userId) {
  return saveVerificationState(guildId, (verification) => {
    const expirations = { ...(verification.expirations || {}) };
    delete expirations[String(userId)];
    return { expirations };
  });
}

function getExpiredVerificationUsers(guildId, nowIsoValue) {
  const verification = getVerificationState(guildId);
  const expirations = verification.expirations || {};
  const now = Date.parse(nowIsoValue || nowIso());

  return Object.entries(expirations)
    .filter(([, iso]) => {
      const expiry = Date.parse(iso);
      return Number.isFinite(expiry) && expiry <= now;
    })
    .map(([userId]) => userId);
}

function getVerificationChallenge(guildId, userId) {
  const verification = getVerificationState(guildId);
  const challenges = verification.challenges || {};
  const challenge = challenges[String(userId)];
  return challenge && typeof challenge === 'object' ? challenge : null;
}

function saveVerificationChallenge(guildId, userId, payload) {
  const normalizedUserId = String(userId);
  return saveVerificationState(guildId, (verification) => {
    const challenges = { ...(verification.challenges || {}) };
    challenges[normalizedUserId] = {
      roleId: String(payload.roleId),
      code: String(payload.code),
      attempts: Number(payload.attempts || 0),
      expiresAt: payload.expiresAt || null,
    };

    return { challenges };
  });
}

function removeVerificationChallenge(guildId, userId) {
  const normalizedUserId = String(userId);
  return saveVerificationState(guildId, (verification) => {
    const challenges = { ...(verification.challenges || {}) };
    delete challenges[normalizedUserId];
    return { challenges };
  });
}

function pruneExpiredVerificationChallenges(guildId, nowIsoValue) {
  const now = Date.parse(nowIsoValue || nowIso());
  const verification = getVerificationState(guildId);
  const current = verification.challenges || {};
  const entries = Object.entries(current);

  if (entries.length === 0) {
    return verification;
  }

  let changed = false;
  const next = {};

  for (const [userId, challenge] of entries) {
    const expiresAt = Date.parse(challenge?.expiresAt || '');
    if (!Number.isFinite(expiresAt) || expiresAt <= now) {
      changed = true;
      continue;
    }

    next[userId] = challenge;
  }

  if (!changed) {
    return verification;
  }

  return saveVerificationState(guildId, {
    challenges: next,
  });
}

function getVerificationLockout(guildId, userId) {
  const verification = getVerificationState(guildId);
  const lockouts = verification.lockouts || {};
  const lockoutUntil = lockouts[String(userId)];
  return typeof lockoutUntil === 'string' ? lockoutUntil : null;
}

function setVerificationLockout(guildId, userId, lockoutUntilIso) {
  const normalizedUserId = String(userId);
  return saveVerificationState(guildId, (verification) => {
    const lockouts = { ...(verification.lockouts || {}) };
    lockouts[normalizedUserId] = lockoutUntilIso;
    return { lockouts };
  });
}

function removeVerificationLockout(guildId, userId) {
  const normalizedUserId = String(userId);
  return saveVerificationState(guildId, (verification) => {
    const lockouts = { ...(verification.lockouts || {}) };
    delete lockouts[normalizedUserId];
    return { lockouts };
  });
}

function pruneExpiredVerificationLockouts(guildId, nowIsoValue) {
  const now = Date.parse(nowIsoValue || nowIso());
  const verification = getVerificationState(guildId);
  const current = verification.lockouts || {};
  const entries = Object.entries(current);

  if (entries.length === 0) {
    return verification;
  }

  let changed = false;
  const next = {};

  for (const [userId, lockoutUntil] of entries) {
    const lockoutTs = Date.parse(lockoutUntil || '');
    if (!Number.isFinite(lockoutTs) || lockoutTs <= now) {
      changed = true;
      continue;
    }

    next[userId] = lockoutUntil;
  }

  if (!changed) {
    return verification;
  }

  return saveVerificationState(guildId, {
    lockouts: next,
  });
}

function getLogsState(guildId) {
  const state = loadGuildState(guildId);
  return deepMergeState({
    enabled: false,
    channelId: null,
    retentionDays: 30,
    forwardMode: 'all',
    includeBots: false,
    disabledCategories: [],
  }, state.logs || {});
}

function saveLogsState(guildId, patchOrUpdater) {
  return saveGuildState(guildId, (current) => {
    const currentLogs = current.logs || getLogsState(guildId);
    const patch = typeof patchOrUpdater === 'function'
      ? patchOrUpdater(currentLogs, current)
      : patchOrUpdater;
    return {
      logs: patch || {},
    };
  });
}

module.exports = {
  initServerStateStore,
  loadGuildState,
  saveGuildState,
  markGuildOnline,
  markGuildOffline,
  getAutoCleanState,
  saveAutoCleanState,
  markCleanupResult,
  setNextRunAt,
  listQueuedAutoCleanChannels,
  addQueuedAutoCleanChannel,
  removeQueuedAutoCleanChannel,
  removeQueuedAutoCleanChannels,
  migrateLegacyServerCache,
  getAnnouncementsState,
  saveAnnouncementsState,
  getVerificationState,
  saveVerificationState,
  setVerificationPanel,
  addVerificationException,
  removeVerificationException,
  listVerificationExceptions,
  setUserVerificationExpiry,
  removeUserVerificationExpiry,
  getExpiredVerificationUsers,
  getVerificationChallenge,
  saveVerificationChallenge,
  removeVerificationChallenge,
  pruneExpiredVerificationChallenges,
  getVerificationLockout,
  setVerificationLockout,
  removeVerificationLockout,
  pruneExpiredVerificationLockouts,
  getLogsState,
  saveLogsState,
};
