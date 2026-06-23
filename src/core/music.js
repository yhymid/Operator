const fs = require('fs');
const path = require('path');
const { LavalinkManager } = require('lavalink-client');
const { resolveOperatorPaths } = require('./paths');

const DEFAULT_LAVALINK_HOST = '127.0.0.1';
const DEFAULT_LAVALINK_PORT = 2333;
const DEFAULT_LAVALINK_PASSWORD = 'youshallnotpass';
const DEFAULT_SEARCH_SOURCE = 'ytmsearch';
const DEFAULT_SPOTIFY_MIRROR_SOURCES = ['ytsearch', 'ytmsearch', 'scsearch'];
const DEFAULT_SPOTIFY_PLAYLIST_MIRROR_PREFETCH = 8;
const DEFAULT_RECOVERY_SOURCES = ['ytsearch', 'ytmsearch', 'scsearch'];
const DEFAULT_SPOTIFY_PROGRESSIVE_INITIAL_TRACKS = 3;
const DEFAULT_SPOTIFY_PROGRESSIVE_INITIAL_PROBE_LIMIT = 24;
const DEFAULT_SPOTIFY_PROGRESSIVE_BACKFILL_BATCH_SIZE = 5;
const DEFAULT_SPOTIFY_PROGRESSIVE_LOG_EVERY = 25;
const SPOTIFY_PRIMARY_SOURCE_NAMES = new Set(['youtube', 'youtubemusic']);
const SPOTIFY_FALLBACK_SOURCE_NAMES = new Set(['soundcloud']);
const SPOTIFY_MIRROR_MIN_SCORE = 70;
const SPOTIFY_MIRROR_MIN_TITLE_OVERLAP = 0.46;
const SPOTIFY_MIRROR_MIN_AUTHOR_OVERLAP = 0.24;
const SPOTIFY_MIRROR_MAX_DURATION_DIFF_SEC = 22;
const SPOTIFY_MIRROR_MAX_DURATION_DIFF_RATIO = 0.14;
const MATCH_VARIANT_MARKERS = [
  'tribute',
  'cover',
  'karaoke',
  'remix',
  'bootleg',
  'edit',
  'mashup',
  'slowed',
  'sped',
  'nightcore',
  'instrumental',
  '8d',
  'reverb',
  'live',
];
const IDLE_DISCONNECT_MS = 2 * 60 * 1000;
const TRACK_RECOVERY_COOLDOWN_MS = 90 * 1000;
const RECOVERABLE_TRACK_ERROR_PATTERN = /(requires.*login|sign\s*in|forbidden|status code[:\s]*403|video player configuration|age[-\s]?restricted|not available|unavailable|something went wrong while looking up the track|failed to retrieve secret from spotify)/i;
const DEFAULT_QUEUE_STATE_DIR = resolveOperatorPaths().musicQueueDir;
const MAX_VOICE_CHANNEL_STATUS_LENGTH = 120;
const VOICE_CHANNEL_STATUS_ROUTE = (channelId) => `/channels/${channelId}/voice-status`;

function envBoolean(name, fallback = false) {
  const raw = String(process.env[name] || '').trim();
  if (!raw) return fallback;
  return /^(1|true|yes|on)$/i.test(raw);
}

function envNumber(name, fallback) {
  const parsed = Number(process.env[name]);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function compactWhitespace(value) {
  return String(value || '').replace(/\s+/g, ' ').trim();
}

function truncateInline(value, maxLength = 80) {
  const normalized = compactWhitespace(value);
  if (!normalized) return '';
  if (normalized.length <= maxLength) return normalized;
  return `${normalized.slice(0, Math.max(0, maxLength - 3)).trimEnd()}...`;
}

function isHttpUrl(input) {
  const raw = String(input || '').trim();
  if (!raw) return false;

  try {
    const parsed = new URL(raw);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:';
  } catch {
    return false;
  }
}

function extractYoutubeVideoId(input) {
  if (!isHttpUrl(input)) return null;
  try {
    const parsed = new URL(String(input).trim());
    const host = String(parsed.hostname || '').toLowerCase();
    const pathname = String(parsed.pathname || '');

    if (host === 'youtu.be') {
      const id = pathname.replace(/^\/+/, '').split('/')[0];
      return id || null;
    }

    if (host.endsWith('youtube.com') || host.endsWith('youtube-nocookie.com')) {
      if (pathname === '/watch') {
        const id = parsed.searchParams.get('v');
        return id ? String(id).trim() : null;
      }
      if (pathname.startsWith('/shorts/')) {
        const id = pathname.replace('/shorts/', '').split('/')[0];
        return id || null;
      }
      if (pathname.startsWith('/embed/')) {
        const id = pathname.replace('/embed/', '').split('/')[0];
        return id || null;
      }
    }
  } catch {
    return null;
  }
  return null;
}

function buildYoutubeResolveCandidates(rawUrl) {
  const raw = String(rawUrl || '').trim();
  const id = extractYoutubeVideoId(raw);
  if (!id) return [raw];

  const candidates = [
    raw,
    `https://www.youtube.com/watch?v=${id}`,
    { query: id, source: 'ytsearch' },
    { query: id, source: 'ytmsearch' },
  ];

  const deduped = [];
  const seen = new Set();
  for (const candidate of candidates) {
    const key = typeof candidate === 'string'
      ? `str:${candidate}`
      : `obj:${candidate.source}:${candidate.query}`;
    if (seen.has(key)) continue;
    seen.add(key);
    deduped.push(candidate);
  }
  return deduped;
}

function parseCsvList(value, fallback = []) {
  const parsed = String(value || '')
    .split(',')
    .map((item) => item.trim().toLowerCase())
    .filter(Boolean);

  if (parsed.length === 0) {
    return [...fallback];
  }
  return [...new Set(parsed)];
}

function isSpotifyTrack(track) {
  const sourceName = String(track?.info?.sourceName || '').toLowerCase();
  if (sourceName === 'spotify') return true;

  const uri = String(track?.info?.uri || '');
  return /open\.spotify\.com\//i.test(uri);
}

function getTrackIsrc(track) {
  return compactWhitespace(String(track?.info?.isrc || ''));
}

function buildTrackSeed(track) {
  const info = track?.info || {};
  const author = String(info.author || '').toLowerCase() === 'unknown artist'
    ? ''
    : String(info.author || '');
  const title = String(info.title || '');
  return compactWhitespace(`${author} ${title}`);
}

function normalizeForMatch(value) {
  return String(value || '')
    .toLowerCase()
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .replace(/[^a-z0-9]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

function tokenizeForMatch(value) {
  return normalizeForMatch(value).split(' ').filter(Boolean);
}

function tokenOverlapRatio(left, right) {
  const leftTokens = tokenizeForMatch(left);
  const rightTokens = new Set(tokenizeForMatch(right));
  if (leftTokens.length === 0 || rightTokens.size === 0) return 0;

  let matches = 0;
  for (const token of leftTokens) {
    if (rightTokens.has(token)) matches += 1;
  }

  return matches / leftTokens.length;
}

function hasVariantMarker(value) {
  const normalized = normalizeForMatch(value);
  if (!normalized) return false;
  return MATCH_VARIANT_MARKERS.some((marker) => normalized.includes(marker));
}

function isLikelyPreviewLength(originalDurationSec, candidateDurationSec) {
  if (originalDurationSec <= 0 || candidateDurationSec <= 0) return false;
  if (originalDurationSec < 120) return false;
  return candidateDurationSec >= 25 && candidateDurationSec <= 70;
}

function isPreviewTrack(track) {
  const pluginInfo = track?.pluginInfo || {};
  const info = track?.info || {};

  return pluginInfo?.isPreview === true || info?.isPreview === true;
}

function isSpotifyMirrorCandidateSource(sourceName) {
  const normalized = String(sourceName || '').toLowerCase();
  return SPOTIFY_PRIMARY_SOURCE_NAMES.has(normalized) || SPOTIFY_FALLBACK_SOURCE_NAMES.has(normalized);
}

function normalizeTrack(track) {
  const info = track?.info || {};
  const title = compactWhitespace([
    info.author && info.author !== 'Unknown artist' ? info.author : '',
    info.title || 'Unknown track',
  ].filter(Boolean).join(' - '));

  return {
    title: title || 'Unknown track',
    durationSec: Math.round(Number(info.duration || 0) / 1000),
    url: String(info.uri || ''),
    sourcePlatform: String(info.sourceName || 'lavalink'),
    lavalinkTrack: track,
  };
}

function normalizeRepeatMode(value) {
  return ['off', 'track', 'queue'].includes(value) ? value : 'off';
}

function writeJsonFileAtomic(filePath, payload) {
  const dirPath = path.dirname(filePath);
  const tempPath = path.join(
    dirPath,
    `.${path.basename(filePath)}.${process.pid}.${Date.now()}.tmp`,
  );

  fs.mkdirSync(dirPath, { recursive: true });

  try {
    fs.writeFileSync(tempPath, JSON.stringify(payload, null, 2), 'utf8');
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

function createDefaultFilterState() {
  return {
    bassboost: false,
    nightcore: false,
    vaporwave: false,
    eightD: false,
    karaoke: false,
    softLimiter: false,
    eqPreset: 'flat',
  };
}

function createFlatEqBands() {
  return Array.from({ length: 15 }, (_v, band) => ({ band, gain: 0 }));
}

function getEqBandsForPreset(preset) {
  if (preset === 'vocal') {
    return [
      { band: 0, gain: -0.08 },
      { band: 1, gain: -0.06 },
      { band: 2, gain: -0.04 },
      { band: 3, gain: 0.02 },
      { band: 4, gain: 0.08 },
      { band: 5, gain: 0.12 },
      { band: 6, gain: 0.15 },
      { band: 7, gain: 0.14 },
      { band: 8, gain: 0.10 },
      { band: 9, gain: 0.06 },
      { band: 10, gain: 0.04 },
      { band: 11, gain: 0.02 },
      { band: 12, gain: 0 },
      { band: 13, gain: -0.02 },
      { band: 14, gain: -0.04 },
    ];
  }

  if (preset === 'treble') {
    return [
      { band: 0, gain: -0.10 },
      { band: 1, gain: -0.08 },
      { band: 2, gain: -0.06 },
      { band: 3, gain: -0.03 },
      { band: 4, gain: 0 },
      { band: 5, gain: 0.02 },
      { band: 6, gain: 0.05 },
      { band: 7, gain: 0.08 },
      { band: 8, gain: 0.12 },
      { band: 9, gain: 0.16 },
      { band: 10, gain: 0.18 },
      { band: 11, gain: 0.18 },
      { band: 12, gain: 0.16 },
      { band: 13, gain: 0.13 },
      { band: 14, gain: 0.10 },
    ];
  }

  return createFlatEqBands();
}

function getTrackFromPublicTrack(track) {
  return track?.lavalinkTrack || track;
}

function getPlaylistTitle(result) {
  return compactWhitespace(
    result?.playlist?.title
      || result?.playlist?.name
      || result?.pluginInfo?.albumName
      || result?.pluginInfo?.identifier
      || 'Playlist',
  );
}

function buildLavalinkNodesFromEnv() {
  const url = String(process.env.LAVALINK_URL || '').trim();
  if (url) {
    const parsed = new URL(url);
    return [{
      id: parsed.username ? decodeURIComponent(parsed.username) : 'operator-main',
      host: parsed.hostname,
      port: Number(parsed.port || (parsed.protocol === 'https:' || parsed.protocol === 'wss:' ? 443 : DEFAULT_LAVALINK_PORT)),
      authorization: parsed.password ? decodeURIComponent(parsed.password) : DEFAULT_LAVALINK_PASSWORD,
      secure: parsed.protocol === 'https:' || parsed.protocol === 'wss:',
    }];
  }

  return [{
    id: String(process.env.LAVALINK_NODE_ID || 'operator-main'),
    host: String(process.env.LAVALINK_HOST || DEFAULT_LAVALINK_HOST),
    port: envNumber('LAVALINK_PORT', DEFAULT_LAVALINK_PORT),
    authorization: String(process.env.LAVALINK_PASSWORD || DEFAULT_LAVALINK_PASSWORD),
    secure: envBoolean('LAVALINK_SECURE', false),
  }];
}

class MusicManager {
  constructor(client, options = {}) {
    this.client = client;
    this.logger = options.logger || null;
    this.defaultSearchSource = String(process.env.LAVALINK_SEARCH_SOURCE || DEFAULT_SEARCH_SOURCE).trim() || DEFAULT_SEARCH_SOURCE;
    this.spotifyMirrorSources = parseCsvList(process.env.SPOTIFY_MIRROR_SOURCES, DEFAULT_SPOTIFY_MIRROR_SOURCES);
    this.recoverySources = parseCsvList(process.env.MUSIC_RECOVERY_SOURCES, DEFAULT_RECOVERY_SOURCES);
    this.spotifyPlaylistMirrorPrefetch = Math.max(
      0,
      Math.floor(envNumber('SPOTIFY_PLAYLIST_MIRROR_PREFETCH', DEFAULT_SPOTIFY_PLAYLIST_MIRROR_PREFETCH)),
    );
    this.spotifyProgressiveInitialTracks = Math.max(
      1,
      Math.floor(envNumber('SPOTIFY_PROGRESSIVE_INITIAL_TRACKS', DEFAULT_SPOTIFY_PROGRESSIVE_INITIAL_TRACKS)),
    );
    this.spotifyProgressiveInitialProbeLimit = Math.max(
      this.spotifyProgressiveInitialTracks,
      Math.floor(envNumber('SPOTIFY_PROGRESSIVE_INITIAL_PROBE_LIMIT', DEFAULT_SPOTIFY_PROGRESSIVE_INITIAL_PROBE_LIMIT)),
    );
    this.spotifyProgressiveBackfillBatchSize = Math.max(
      1,
      Math.floor(envNumber('SPOTIFY_PROGRESSIVE_BACKFILL_BATCH_SIZE', DEFAULT_SPOTIFY_PROGRESSIVE_BACKFILL_BATCH_SIZE)),
    );
    this.spotifyProgressiveLogEvery = Math.max(
      1,
      Math.floor(envNumber('SPOTIFY_PROGRESSIVE_LOG_EVERY', DEFAULT_SPOTIFY_PROGRESSIVE_LOG_EVERY)),
    );
    this.defaultVolume = envNumber('LAVALINK_VOLUME', 80);
    this.idleDisconnectMs = envNumber('MUSIC_IDLE_DISCONNECT_MS', IDLE_DISCONNECT_MS);
    this.idleTimers = new Map();
    this.previousVolumes = new Map();
    this.filterStates = new Map();
    this.autoplayStates = new Map();
    this.trackRecoveryHistory = new Map();
    this.spotifyBackfillJobs = new Map();
    this.queueStateDir = options.queueStateDir || DEFAULT_QUEUE_STATE_DIR;
    this.initialized = false;
    this.initPromise = null;
    this.voiceChannelStatuses = new Map();
    this.voiceStatusSyncPromise = Promise.resolve();
    this.legacyMusicPresenceCleared = false;

    this.manager = new LavalinkManager({
      nodes: buildLavalinkNodesFromEnv(),
      sendToShard: (guildId, payload) => {
        const guild = this.client.guilds.cache.get(guildId);
        guild?.shard?.send(payload);
      },
      autoSkip: true,
      autoMove: true,
      autoSkipOnResolveError: true,
      playerOptions: {
        defaultSearchPlatform: this.defaultSearchSource,
        volumeDecrementer: 1,
        applyVolumeAsFilter: false,
        useUnresolvedData: true,
        onDisconnect: {
          autoReconnect: true,
          destroyPlayer: false,
        },
        onEmptyQueue: {
          destroyAfterMs: this.idleDisconnectMs,
          autoPlayFunction: async (player, track) => {
            await this.handleAutoplay(player, track);
          },
        },
        maxErrorsPerTime: {
          threshold: envNumber('MUSIC_ERROR_THRESHOLD_MS', 120000),
          maxAmount: envNumber('MUSIC_ERROR_MAX_AMOUNT', 30),
        },
      },
      queueOptions: {
        maxPreviousTracks: 25,
      },
    });

    this.manager.nodeManager.on('connect', (node) => {
      this.logger?.info?.(`Music provider ready: Lavalink node ${node.id}`);
    });
    this.manager.nodeManager.on('disconnect', (node, reason) => {
      this.logger?.warn?.(`Lavalink node disconnected (${node.id}): ${reason?.reason || reason?.code || 'unknown'}`);
    });
    this.manager.nodeManager.on('error', (node, error) => {
      this.logger?.error?.(`Lavalink node error (${node?.id || 'unknown'}): ${error.message}`);
    });

    this.manager.on('trackStart', (player, track) => {
      this.clearIdleTimer(player.guildId);
      this.clearTrackRecoveryCooldown(player.guildId, track);
      this.logger?.info?.(`Music track start guild=${player.guildId} title="${track?.info?.title || 'unknown'}"`);
      this.refreshMusicPresence();
    });
    this.manager.on('trackError', (player, track, payload) => {
      this.logger?.warn?.(`Music track error guild=${player.guildId} title="${track?.info?.title || 'unknown'}": ${payload?.exception?.message || 'unknown'}`);
      this.handleTrackErrorRecovery(player, track, payload).catch((error) => {
        this.logger?.warn?.(`Music recovery error guild=${player?.guildId || 'unknown'}: ${error.message}`);
      });
    });
    this.manager.on('trackStuck', (player, track) => {
      this.logger?.warn?.(`Music track stuck guild=${player.guildId} title="${track?.info?.title || 'unknown'}"`);
    });
    this.manager.on('queueEnd', (player) => {
      this.scheduleIdleDestroy(player.guildId);
      this.refreshMusicPresence();
    });
    this.manager.on('playerDestroy', (player) => {
      this.clearIdleTimer(player.guildId);
      this.previousVolumes.delete(String(player.guildId));
      this.clearFilterState(player.guildId);
      this.autoplayStates.delete(String(player.guildId));
      this.trackRecoveryHistory.delete(String(player.guildId));
      this.cancelSpotifyBackfill(player.guildId, 'player destroy');
      this.refreshMusicPresence();
    });

    this.client.on('raw', (payload) => {
      if (!this.initialized) return;
      this.manager.sendRawData(payload).catch((error) => {
        this.logger?.warn?.(`Lavalink raw event error: ${error.message}`);
      });
    });
  }

  getFilterState(guildId) {
    const id = String(guildId || '');
    const existing = this.filterStates.get(id);
    if (existing) return existing;

    const state = createDefaultFilterState();
    this.filterStates.set(id, state);
    return state;
  }

  getVoiceChannelStatusText(player) {
    if (!player?.queue?.current) return null;
    if (!player.playing && !player.paused) return null;

    const track = normalizeTrack(player.queue.current);
    if (!track?.title) return null;

    const prefix = player.paused ? 'Pauza: ' : 'Gra: ';
    return truncateInline(`${prefix}${track.title}`, MAX_VOICE_CHANNEL_STATUS_LENGTH);
  }

  clearLegacyMusicPresence() {
    if (!this.client?.user) return;

    try {
      this.client.user.setPresence({
        activities: [],
        status: 'online',
      });
      this.legacyMusicPresenceCleared = true;
    } catch (error) {
      this.logger?.warn?.(`Music presence clear failed: ${error.message}`);
    }
  }

  async setVoiceChannelStatus(channelId, statusText) {
    const normalizedChannelId = String(channelId || '').trim();
    if (!normalizedChannelId) return;

    await this.client.rest.put(VOICE_CHANNEL_STATUS_ROUTE(normalizedChannelId), {
      body: {
        status: statusText ? truncateInline(statusText, MAX_VOICE_CHANNEL_STATUS_LENGTH) : null,
      },
    });
  }

  async syncVoiceChannelStatuses() {
    if (!this.client?.user) return;
    if (!this.legacyMusicPresenceCleared) {
      this.clearLegacyMusicPresence();
    }

    const desiredStatuses = new Map();
    for (const player of this.manager.players.values()) {
      const channelId = String(player?.voiceChannelId || '').trim();
      const statusText = this.getVoiceChannelStatusText(player);
      if (channelId && statusText) {
        desiredStatuses.set(channelId, statusText);
      }
    }

    const channelIds = new Set([
      ...this.voiceChannelStatuses.keys(),
      ...desiredStatuses.keys(),
    ]);

    for (const channelId of channelIds) {
      const nextStatus = desiredStatuses.get(channelId) || null;
      const previousStatus = this.voiceChannelStatuses.get(channelId) || null;
      if (previousStatus === nextStatus) continue;

      try {
        await this.setVoiceChannelStatus(channelId, nextStatus);
        if (nextStatus) {
          this.voiceChannelStatuses.set(channelId, nextStatus);
        } else {
          this.voiceChannelStatuses.delete(channelId);
        }
      } catch (error) {
        this.logger?.warn?.(`Music voice status update failed channel=${channelId}: ${error.message}`);
      }
    }
  }

  // Legacy name kept to avoid churn in call sites while the display moved from presence to voice channel status.
  refreshMusicPresence() {
    this.voiceStatusSyncPromise = this.voiceStatusSyncPromise
      .catch(() => {})
      .then(() => this.syncVoiceChannelStatuses())
      .catch((error) => {
        this.logger?.warn?.(`Music voice status refresh failed: ${error.message}`);
      });
  }

  async clearVoiceChannelStatus(channelId) {
    const normalizedChannelId = String(channelId || '').trim();
    if (!normalizedChannelId) return;

    this.voiceStatusSyncPromise = this.voiceStatusSyncPromise
      .catch(() => {})
      .then(async () => {
        if (!this.legacyMusicPresenceCleared) {
          this.clearLegacyMusicPresence();
        }
        await this.setVoiceChannelStatus(normalizedChannelId, null);
        this.voiceChannelStatuses.delete(normalizedChannelId);
      })
      .catch((error) => {
        this.logger?.warn?.(`Music voice status clear failed channel=${normalizedChannelId}: ${error.message}`);
      });

    await this.voiceStatusSyncPromise;
  }

  setFilterState(guildId, patch) {
    const id = String(guildId || '');
    const state = {
      ...this.getFilterState(id),
      ...patch,
    };
    this.filterStates.set(id, state);
    return state;
  }

  clearFilterState(guildId) {
    this.filterStates.delete(String(guildId || ''));
  }

  getAutoplayEnabled(guildId) {
    return this.autoplayStates.get(String(guildId || '')) === true;
  }

  setAutoplayEnabled(guildId, enabled) {
    const id = String(guildId || '');
    const value = enabled === true;
    if (value) {
      this.autoplayStates.set(id, true);
    } else {
      this.autoplayStates.delete(id);
    }
    return value;
  }

  toggleAutoplay(guildId) {
    const next = !this.getAutoplayEnabled(guildId);
    this.setAutoplayEnabled(guildId, next);
    return next;
  }

  ensureQueueStateDir() {
    fs.mkdirSync(this.queueStateDir, { recursive: true });
  }

  getQueueStatePath(guildId) {
    return path.join(this.queueStateDir, `${String(guildId || '')}.json`);
  }

  async handleAutoplay(player, track) {
    const guildId = String(player?.guildId || '');
    if (!guildId || !this.getAutoplayEnabled(guildId)) return;

    const info = track?.info || {};
    const seed = compactWhitespace(`${info.author || ''} ${info.title || ''}`);
    if (!seed) return;

    try {
      const result = await player.search(
        { query: seed, source: this.defaultSearchSource },
        track?.requester || undefined,
        false,
      );
      const tracks = Array.isArray(result?.tracks) ? result.tracks : [];
      if (tracks.length === 0) return;

      const currentUri = String(info.uri || '');
      const nextTrack = tracks.find((candidate) => String(candidate?.info?.uri || '') !== currentUri) || tracks[0];
      if (!nextTrack) return;

      await player.queue.add(nextTrack);
      this.logger?.info?.(`Autoplay dodal utwor guild=${guildId} title="${nextTrack?.info?.title || 'unknown'}"`);
    } catch (error) {
      this.logger?.warn?.(`Autoplay error guild=${guildId}: ${error.message}`);
    }
  }

  getTrackRecoveryKey(track) {
    const info = track?.info || {};
    return [
      String(info.sourceName || '').toLowerCase(),
      String(info.identifier || ''),
      String(info.uri || ''),
      buildTrackSeed(track).toLowerCase(),
    ].join('|');
  }

  clearTrackRecoveryCooldown(guildId, track = null) {
    const id = String(guildId || '');
    const history = this.trackRecoveryHistory.get(id);
    if (!history) return;

    if (!track) {
      this.trackRecoveryHistory.delete(id);
      return;
    }

    history.delete(this.getTrackRecoveryKey(track));
    if (history.size === 0) {
      this.trackRecoveryHistory.delete(id);
    }
  }

  shouldSkipRecovery(guildId, track) {
    const id = String(guildId || '');
    if (!id || !track) return true;

    const key = this.getTrackRecoveryKey(track);
    const now = Date.now();
    const history = this.trackRecoveryHistory.get(id);
    if (history && history.has(key)) {
      const elapsed = now - Number(history.get(key) || 0);
      if (elapsed < TRACK_RECOVERY_COOLDOWN_MS) {
        return true;
      }
    }

    const nextHistory = history || new Map();
    nextHistory.set(key, now);
    this.trackRecoveryHistory.set(id, nextHistory);
    return false;
  }

  isRecoverableTrackError(payload) {
    const message = String(payload?.exception?.message || '');
    const cause = String(payload?.exception?.cause || '');
    return RECOVERABLE_TRACK_ERROR_PATTERN.test(`${message} ${cause}`);
  }

  isLikelyRecoverableSourceTrack(track) {
    const source = String(track?.info?.sourceName || '').toLowerCase();
    return source === 'spotify';
  }

  buildSpotifyMirrorCandidates(track) {
    const info = track?.info || {};
    const isrc = getTrackIsrc(track);
    const seed = buildTrackSeed(track);
    const title = compactWhitespace(String(info.title || ''));
    const author = compactWhitespace(String(info.author || ''));
    const exactSeed = compactWhitespace(`${author} ${title}`);
    const candidates = [];
    const seen = new Set();

    for (const source of this.spotifyMirrorSources) {
      const normalizedSource = String(source || '').trim().toLowerCase();
      if (!normalizedSource) continue;
      const isIsrcSource = normalizedSource.endsWith('isrc');

      const canUseIsrc = isrc && isIsrcSource;
      if (canUseIsrc) {
        const key = `${normalizedSource}|isrc|${isrc}`;
        if (!seen.has(key)) {
          candidates.push({
            source: normalizedSource,
            query: isrc,
            label: `${normalizedSource}:${isrc}`,
          });
          seen.add(key);
        }
        continue;
      }
      if (isIsrcSource) {
        continue;
      }

      if ((normalizedSource === 'dzsearch' || normalizedSource === 'scsearch') && exactSeed) {
        const key = `${normalizedSource}|exact|${exactSeed.toLowerCase()}`;
        if (!seen.has(key)) {
          candidates.push({
            source: normalizedSource,
            query: exactSeed,
            label: `${normalizedSource}:${exactSeed}`,
          });
          seen.add(key);
        }
      }

      if (seed) {
        const key = `${normalizedSource}|seed|${seed.toLowerCase()}`;
        if (!seen.has(key)) {
          candidates.push({
            source: normalizedSource,
            query: seed,
            label: `${normalizedSource}:${seed}`,
          });
          seen.add(key);
        }
      }
    }

    return candidates;
  }

  scoreMirroredCandidate(originalTrack, candidateTrack) {
    const originalInfo = originalTrack?.info || {};
    const candidateInfo = candidateTrack?.info || {};
    if (!candidateInfo) return -100000;
    if (isSpotifyTrack(candidateTrack)) return -100000;
    if (candidateInfo.isStream === true) return -5000;
    if (isPreviewTrack(candidateTrack)) return -100000;

    const originalTitle = String(originalInfo.title || '');
    const originalAuthor = String(originalInfo.author || '');
    const candidateTitle = String(candidateInfo.title || '');
    const candidateAuthor = String(candidateInfo.author || '');
    const originalDurationSec = Math.round(Number(originalInfo.duration || 0) / 1000);
    const candidateDurationSec = Math.round(Number(candidateInfo.duration || 0) / 1000);

    let score = 0;

    const titleOverlap = tokenOverlapRatio(originalTitle, candidateTitle);
    const authorOverlap = tokenOverlapRatio(originalAuthor, candidateAuthor);
    score += Math.round(titleOverlap * 65);
    score += Math.round(authorOverlap * 45);

    const normalizedOriginalTitle = normalizeForMatch(originalTitle);
    const normalizedCandidateTitle = normalizeForMatch(candidateTitle);
    if (normalizedOriginalTitle && normalizedOriginalTitle === normalizedCandidateTitle) {
      score += 30;
    }
    if (normalizedOriginalTitle && normalizedCandidateTitle.includes(normalizedOriginalTitle)) {
      score += 10;
    }

    if (originalDurationSec > 0 && candidateDurationSec > 0) {
      if (isLikelyPreviewLength(originalDurationSec, candidateDurationSec)) {
        return -10000;
      }
      const diff = Math.abs(originalDurationSec - candidateDurationSec);
      if (diff <= 2) score += 25;
      else if (diff <= 5) score += 20;
      else if (diff <= 10) score += 14;
      else if (diff <= 20) score += 8;
      else if (diff <= 35) score += 3;
      else if (diff <= 55) score -= 8;
      else score -= 20;
    }

    const originalHasVariant = hasVariantMarker(`${originalTitle} ${originalAuthor}`);
    const candidateHasVariant = hasVariantMarker(`${candidateTitle} ${candidateAuthor}`);
    if (!originalHasVariant && candidateHasVariant) {
      score -= 30;
    }

    const sourceName = String(candidateInfo.sourceName || '').toLowerCase();
    if (sourceName === 'deezer') score += 14;
    if (sourceName === 'soundcloud') score += 3;
    if (sourceName === 'youtube' || sourceName === 'youtubemusic') score -= 8;

    if (authorOverlap < 0.25 && originalAuthor) {
      score -= 15;
    }
    if (titleOverlap < 0.35) {
      score -= 20;
    }

    if (originalAuthor && authorOverlap < 0.2) {
      score -= 25;
    }
    if (titleOverlap < 0.25) {
      score -= 30;
    }

    return score;
  }

  passesSpotifyMirrorThreshold(originalTrack, candidateTrack, score) {
    const originalInfo = originalTrack?.info || {};
    const candidateInfo = candidateTrack?.info || {};

    const titleOverlap = tokenOverlapRatio(originalInfo.title || '', candidateInfo.title || '');
    const authorOverlap = tokenOverlapRatio(originalInfo.author || '', candidateInfo.author || '');
    const originalDurationSec = Math.round(Number(originalInfo.duration || 0) / 1000);
    const candidateDurationSec = Math.round(Number(candidateInfo.duration || 0) / 1000);
    const durationDiffSec = Math.abs(originalDurationSec - candidateDurationSec);
    const durationRatio = originalDurationSec > 0 ? durationDiffSec / originalDurationSec : 1;

    if (score < SPOTIFY_MIRROR_MIN_SCORE) return false;
    if (titleOverlap < SPOTIFY_MIRROR_MIN_TITLE_OVERLAP) return false;
    if ((originalInfo.author || '') && authorOverlap < SPOTIFY_MIRROR_MIN_AUTHOR_OVERLAP) return false;
    if (originalDurationSec <= 0 || candidateDurationSec <= 0) return false;
    if (durationDiffSec > SPOTIFY_MIRROR_MAX_DURATION_DIFF_SEC) return false;
    if (durationRatio > SPOTIFY_MIRROR_MAX_DURATION_DIFF_RATIO) return false;
    return true;
  }

  pickMirroredTrack(resultTracks, originalTrack) {
    const tracks = Array.isArray(resultTracks) ? resultTracks : [];
    if (tracks.length === 0) return null;
    const spotifyUri = String(originalTrack?.info?.uri || '');
    const originalIsSpotify = isSpotifyTrack(originalTrack);
    const ranked = tracks
      .filter((candidate) => candidate?.info && !isSpotifyTrack(candidate))
      .filter((candidate) => !isPreviewTrack(candidate))
      .filter((candidate) => !(spotifyUri && String(candidate.info.uri || '') === spotifyUri))
      .filter((candidate) => {
        if (!originalIsSpotify) return true;
        const sourceName = String(candidate?.info?.sourceName || '').toLowerCase();
        return isSpotifyMirrorCandidateSource(sourceName);
      })
      .map((candidate) => ({
        candidate,
        score: this.scoreMirroredCandidate(originalTrack, candidate),
      }))
      .sort((a, b) => b.score - a.score);

    if (ranked.length === 0) return null;
    const best = ranked[0];
    if (originalIsSpotify && !this.passesSpotifyMirrorThreshold(originalTrack, best.candidate, best.score)) {
      return null;
    }
    if (!originalIsSpotify && best.score < 55) return null;
    return best.candidate;
  }

  async resolveSpotifyMirrorTrack(track, requestedBy, searcher) {
    const candidates = this.buildSpotifyMirrorCandidates(track);
    if (candidates.length === 0) return null;

    for (const candidate of candidates) {
      try {
        const result = await searcher.search(
          { query: candidate.query, source: candidate.source },
          requestedBy || track?.requester || undefined,
          false,
        );
        const mirrored = this.pickMirroredTrack(result?.tracks, track);
        if (mirrored) {
          return mirrored;
        }
      } catch (error) {
        this.logger?.warn?.(`Spotify mirror lookup failed (${candidate.label}): ${error.message}`);
      }
    }

    return null;
  }

  cancelSpotifyBackfill(guildId, reason = 'manual') {
    const id = String(guildId || '');
    const active = this.spotifyBackfillJobs.get(id);
    if (!active) return false;
    active.cancelled = true;
    this.spotifyBackfillJobs.delete(id);
    this.logger?.info?.(`Spotify backfill cancelled guild=${id} reason="${reason}"`);
    return true;
  }

  startSpotifyPlaylistBackfill({
    guildId,
    requestedBy,
    searcher,
    rawTracks,
    playlistTitle,
  }) {
    const id = String(guildId || '');
    const queue = Array.isArray(rawTracks) ? rawTracks.filter(Boolean) : [];
    if (!id || queue.length === 0) return;

    this.cancelSpotifyBackfill(id, 'replace');
    const job = {
      cancelled: false,
      startedAt: Date.now(),
      total: queue.length,
    };
    this.spotifyBackfillJobs.set(id, job);
    this.logger?.info?.(
      `Spotify backfill started guild=${id} playlist="${playlistTitle || 'Playlist'}" pending=${queue.length} batch=${this.spotifyProgressiveBackfillBatchSize}`,
    );

    const flushBatch = async (player, pending) => {
      if (pending.length === 0) return 0;
      const toAdd = pending.splice(0, pending.length);
      await player.queue.add(toAdd);
      return toAdd.length;
    };

    (async () => {
      let processed = 0;
      let added = 0;
      let dropped = 0;
      const pending = [];

      for (const rawTrack of queue) {
        const active = this.spotifyBackfillJobs.get(id);
        if (active !== job || job.cancelled) return;

        const player = this.manager.getPlayer(id);
        if (!player) {
          this.cancelSpotifyBackfill(id, 'player missing');
          return;
        }

        let candidate = null;
        try {
          if (isSpotifyTrack(rawTrack)) {
            candidate = await this.resolveSpotifyMirrorTrack(rawTrack, requestedBy, searcher || player);
          } else {
            candidate = rawTrack;
          }
        } catch (error) {
          this.logger?.warn?.(
            `Spotify backfill resolve failed guild=${id} title="${rawTrack?.info?.title || 'unknown'}": ${error.message}`,
          );
        }

        processed += 1;
        if (candidate) {
          pending.push(candidate);
        } else {
          dropped += 1;
        }

        if (pending.length >= this.spotifyProgressiveBackfillBatchSize) {
          try {
            added += await flushBatch(player, pending);
          } catch (error) {
            this.logger?.warn?.(`Spotify backfill queue add failed guild=${id}: ${error.message}`);
          }
        }

        if (processed % this.spotifyProgressiveLogEvery === 0) {
          this.logger?.info?.(
            `Spotify backfill progress guild=${id} processed=${processed}/${queue.length} added=${added} dropped=${dropped}`,
          );
        }
      }

      const currentPlayer = this.manager.getPlayer(id);
      if (currentPlayer) {
        try {
          added += await flushBatch(currentPlayer, pending);
        } catch (error) {
          this.logger?.warn?.(`Spotify backfill final add failed guild=${id}: ${error.message}`);
        }
      }

      this.logger?.info?.(
        `Spotify backfill done guild=${id} processed=${processed}/${queue.length} added=${added} dropped=${dropped}`,
      );
    })().catch((error) => {
      this.logger?.warn?.(`Spotify backfill worker error guild=${id}: ${error.message}`);
    }).finally(() => {
      const active = this.spotifyBackfillJobs.get(id);
      if (active === job) {
        this.spotifyBackfillJobs.delete(id);
      }
    });
  }

  async enqueueSpotifyPlaylistProgressive({
    guild,
    voiceChannel,
    requestedBy,
    query,
    session: providedSession = null,
  }) {
    this.ensureReady();
    const session = providedSession || await this.ensureSession(guild, voiceChannel);
    const player = session.player;
    const searcher = player || this.manager.nodeManager.leastUsedNodes('players')[0];
    if (!searcher) {
      throw new Error('Brak dostepnego node Lavalink.');
    }

    const result = await searcher.search(query, requestedBy || undefined, true);
    const sourceTracks = Array.isArray(result?.tracks) ? result.tracks : [];
    if (sourceTracks.length === 0) {
      throw new Error('Nie znaleziono utworu.');
    }

    const playlistMode = result.loadType === 'playlist';
    if (!playlistMode) {
      const preparedTracks = await this.mirrorSpotifyTracks(sourceTracks, {
        playlistMode: false,
        requestedBy,
        searcher,
      });
      if (preparedTracks.length === 0) {
        throw new Error('Nie znaleziono odtwarzalnego wariantu utworu.');
      }
      const enqueueResult = await this.enqueueMany({
        guild,
        voiceChannel,
        requestedBy,
        tracks: preparedTracks,
        session,
      });
      return {
        mode: 'direct',
        playlistTitle: getPlaylistTitle(result),
        totalSourceTracks: sourceTracks.length,
        initialAdded: enqueueResult.added,
        started: enqueueResult.started,
        startedTrack: enqueueResult.startedTrack,
        pendingBackfill: 0,
      };
    }

    this.cancelSpotifyBackfill(guild.id, 'new spotify playlist');

    const initialTarget = this.spotifyProgressiveInitialTracks;
    const initialProbeLimit = Math.min(sourceTracks.length, this.spotifyProgressiveInitialProbeLimit);
    const immediateTracks = [];
    const deferredTracks = [];
    let inspected = 0;

    for (const rawTrack of sourceTracks) {
      const shouldProbeNow = inspected < initialProbeLimit && immediateTracks.length < initialTarget;
      if (!shouldProbeNow) {
        deferredTracks.push(rawTrack);
        continue;
      }

      inspected += 1;
      if (!isSpotifyTrack(rawTrack)) {
        immediateTracks.push(rawTrack);
        continue;
      }

      const mirroredTrack = await this.resolveSpotifyMirrorTrack(rawTrack, requestedBy, searcher);
      if (mirroredTrack) {
        immediateTracks.push(mirroredTrack);
      } else {
        deferredTracks.push(rawTrack);
      }
    }

    if (immediateTracks.length === 0) {
      throw new Error('Nie udalo sie szybko dopasowac poczatkowych utworow z tej playlisty Spotify.');
    }

    const initialEnqueue = await this.enqueueMany({
      guild,
      voiceChannel,
      requestedBy,
      tracks: immediateTracks,
      session,
    });

    const playlistTitle = getPlaylistTitle(result);
    if (deferredTracks.length > 0) {
      this.startSpotifyPlaylistBackfill({
        guildId: guild.id,
        requestedBy,
        searcher,
        rawTracks: deferredTracks,
        playlistTitle,
      });
    }

    return {
      mode: 'progressive',
      playlistTitle,
      totalSourceTracks: sourceTracks.length,
      initialAdded: initialEnqueue.added,
      started: initialEnqueue.started,
      startedTrack: initialEnqueue.startedTrack,
      pendingBackfill: deferredTracks.length,
    };
  }

  async mirrorSpotifyTracks(tracks, { playlistMode, requestedBy, searcher }) {
    const mirroredTracks = [];
    let replacedCount = 0;
    let droppedSpotifyCount = 0;

    for (let index = 0; index < tracks.length; index += 1) {
      const originalTrack = tracks[index];
      if (!isSpotifyTrack(originalTrack)) {
        mirroredTracks.push(originalTrack);
        continue;
      }

      const mirroredTrack = await this.resolveSpotifyMirrorTrack(originalTrack, requestedBy, searcher);
      if (mirroredTrack) {
        mirroredTracks.push(mirroredTrack);
        replacedCount += 1;
        continue;
      }

      if (playlistMode) {
        droppedSpotifyCount += 1;
        this.logger?.warn?.(
          `Spotify mirror dropped track (no valid candidate) title="${originalTrack?.info?.title || 'unknown'}" author="${originalTrack?.info?.author || 'unknown'}"`,
        );
        continue;
      }

      throw new Error(
        'Nie udalo sie zmirrorowac utworu Spotify do zrodla odtwarzania. Sprobuj innego utworu albo ponow za chwile.',
      );
    }

    if (replacedCount > 0) {
      this.logger?.info?.(`Spotify mirror replaced tracks: ${replacedCount}`);
    }
    if (droppedSpotifyCount > 0) {
      this.logger?.warn?.(`Spotify mirror dropped ${droppedSpotifyCount} track(s) from playlist`);
    }

    return mirroredTracks;
  }

  async handleTrackErrorRecovery(player, track, payload) {
    if (!player || !track || !payload) return;
    if (!this.isRecoverableTrackError(payload)) return;
    if (!this.isLikelyRecoverableSourceTrack(track)) return;
    if (this.shouldSkipRecovery(player.guildId, track)) return;

    const seed = buildTrackSeed(track);
    const isrc = getTrackIsrc(track);
    if (!seed) return;

    const requester = track?.requester || undefined;
    for (const source of this.recoverySources) {
      const queries = [];
      const isIsrcSource = String(source || '').toLowerCase().endsWith('isrc');
      if (isIsrcSource) {
        if (isrc) {
          queries.push(isrc);
        }
      } else if (source === 'scsearch') {
        const title = compactWhitespace(String(track?.info?.title || ''));
        const author = compactWhitespace(String(track?.info?.author || ''));
        if (title && author) {
          queries.push(`"${author}" "${title}"`);
        }
      } else if (source === 'dzsearch') {
        const title = compactWhitespace(String(track?.info?.title || ''));
        const author = compactWhitespace(String(track?.info?.author || ''));
        if (title && author) {
          queries.push(`${author} ${title}`);
        }
      }
      if (!isIsrcSource) {
        queries.push(seed);
      }

      let recovered = false;
      for (const query of queries) {
        if (!query) continue;
        try {
          const result = await player.search({ query, source }, requester, false);
          const candidate = this.pickMirroredTrack(result?.tracks, track);
          if (!candidate) continue;

          await player.queue.add(candidate, 0);
          if (!player.playing && !player.queue.current) {
            await player.play();
          }
          this.logger?.warn?.(
            `Music recovery queued fallback guild=${player.guildId} source=${source} scoreQuery="${query}" title="${candidate?.info?.title || 'unknown'}"`,
          );
          recovered = true;
          break;
        } catch (error) {
          this.logger?.warn?.(
            `Music recovery lookup failed guild=${player.guildId} source=${source} query="${query}": ${error.message}`,
          );
        }
      }

      if (recovered) {
        return;
      }
    }
  }

  async initializeProviders() {
    if (this.initPromise) return this.initPromise;
    this.initPromise = this.initializeLavalink();
    return this.initPromise;
  }

  async initializeLavalink() {
    if (!this.client.user) {
      throw new Error('Discord client is not ready.');
    }

    try {
      await this.manager.init({
        id: this.client.user.id,
        username: this.client.user.username,
      });
      this.initialized = true;
      this.refreshMusicPresence();
      return true;
    } catch (error) {
      this.logger?.error?.(`Music provider unavailable: Lavalink (${error.message})`);
      this.initialized = false;
      this.refreshMusicPresence();
      return false;
    }
  }

  ensureReady() {
    if (!this.initialized || !this.manager.useable) {
      throw new Error('Lavalink nie jest polaczony. Sprawdz LAVALINK_HOST/LAVALINK_PORT/LAVALINK_PASSWORD i czy Lavalink v4 dziala.');
    }
  }

  async resolveTracks(query, requestedBy = null, player = null) {
    this.ensureReady();
    const raw = compactWhitespace(query);
    if (!raw) {
      throw new Error('Podaj link lub fraze do wyszukania.');
    }

    const isUrlQuery = isHttpUrl(raw);
    const searcher = player || this.manager.nodeManager.leastUsedNodes('players')[0];
    if (!searcher) {
      throw new Error('Brak dostepnego node Lavalink.');
    }

    let result = null;
    let tracks = [];
    if (!isUrlQuery) {
      result = await searcher.search({ query: raw, source: this.defaultSearchSource }, requestedBy || undefined, true);
      tracks = Array.isArray(result?.tracks) ? result.tracks : [];
    } else {
      const youtubeCandidates = buildYoutubeResolveCandidates(raw);
      let lastError = null;
      for (const candidate of youtubeCandidates) {
        try {
          const current = await searcher.search(candidate, requestedBy || undefined, true);
          const currentTracks = Array.isArray(current?.tracks) ? current.tracks : [];
          if (currentTracks.length > 0) {
            result = current;
            tracks = currentTracks;
            break;
          }
        } catch (error) {
          lastError = error;
        }
      }
      if (tracks.length === 0 && lastError) {
        throw lastError;
      }
    }

    if (tracks.length === 0) {
      throw new Error('Nie znaleziono utworu.');
    }

    const playlistMode = result.loadType === 'playlist';
    const preparedTracks = await this.mirrorSpotifyTracks(tracks, {
      playlistMode,
      requestedBy,
      searcher,
    });
    if (preparedTracks.length === 0) {
      throw new Error(
        playlistMode
          ? 'Nie udalo sie zmirrorowac utworow z tej playlisty Spotify do odtwarzalnych zrodel.'
          : 'Nie znaleziono odtwarzalnego wariantu utworu.',
      );
    }

    return {
      type: playlistMode ? 'playlist' : 'single',
      playlistTitle: playlistMode ? getPlaylistTitle(result) : null,
      tracks: preparedTracks.map(normalizeTrack),
    };
  }

  async ensureSession(guild, voiceChannel) {
    this.ensureReady();
    if (!guild?.id || !voiceChannel?.id) {
      throw new Error('Brak guild albo voice channel.');
    }

    let replacedIdlePlayer = false;
    let player = this.manager.getPlayer(guild.id);
    if (player && player.voiceChannelId && player.voiceChannelId !== voiceChannel.id) {
      const hasMusic = Boolean(player.queue.current || player.queue.tracks.length || player.playing);
      if (hasMusic) {
        throw new Error(`Bot odtwarza juz muzyke na kanale <#${player.voiceChannelId}>.`);
      }
      await player.destroy('voice channel changed', true).catch(() => {});
      player = null;
      replacedIdlePlayer = true;
    }

    const createdPlayer = !player;
    if (!player) {
      player = this.manager.createPlayer({
        guildId: guild.id,
        voiceChannelId: voiceChannel.id,
        textChannelId: null,
        selfDeaf: true,
        selfMute: false,
        volume: this.defaultVolume,
      });
    }

    this.clearIdleTimer(guild.id);
    if (!player.connected) {
      try {
        await player.connect();
      } catch (error) {
        if (createdPlayer) {
          error.musicFreshSessionCleaned = await this.cleanupFreshSession(
            {
              guildId: guild.id,
              voiceChannelId: voiceChannel.id,
              player,
              createdPlayer: true,
              replacedIdlePlayer,
            },
            'music connect failed',
          );
        }
        throw error;
      }
    }

    return {
      guildId: guild.id,
      voiceChannelId: voiceChannel.id,
      player,
      createdPlayer,
      replacedIdlePlayer,
    };
  }

  async cleanupFreshSession(session, reason = 'music command failed') {
    if (!session?.createdPlayer || !session?.guildId || !session?.player) {
      return false;
    }

    const guildId = String(session.guildId);
    const currentPlayer = this.manager.getPlayer(guildId);
    if (!currentPlayer || currentPlayer !== session.player) {
      return false;
    }

    this.cancelSpotifyBackfill(guildId, reason);
    this.clearIdleTimer(guildId);
    this.previousVolumes.delete(guildId);
    this.clearFilterState(guildId);
    this.autoplayStates.delete(guildId);

    await this.clearVoiceChannelStatus(currentPlayer.voiceChannelId).catch(() => {});
    await currentPlayer.destroy(reason, true).catch((error) => {
      this.logger?.warn?.(`Music fresh session cleanup error guild=${guildId}: ${error.message}`);
    });
    this.refreshMusicPresence();
    return true;
  }

  async enqueue({ guild, voiceChannel, requestedBy, track, session: providedSession = null }) {
    const session = providedSession || await this.ensureSession(guild, voiceChannel);
    const player = session.player;
    const lavalinkTrack = getTrackFromPublicTrack(track);
    const publicTrack = normalizeTrack(lavalinkTrack);
    const wasIdle = !player.playing && !player.queue.current;
    const position = player.queue.tracks.length + 1;

    await player.queue.add(lavalinkTrack);
    if (wasIdle) {
      await player.play();
    }

    return {
      started: wasIdle,
      position,
      track: publicTrack,
      requestedBy,
    };
  }

  async enqueueMany({ guild, voiceChannel, requestedBy, tracks, session: providedSession = null }) {
    const session = providedSession || await this.ensureSession(guild, voiceChannel);
    const player = session.player;
    const lavalinkTracks = (Array.isArray(tracks) ? tracks : [])
      .map(getTrackFromPublicTrack)
      .filter(Boolean);
    if (lavalinkTracks.length === 0) {
      throw new Error('Nie znaleziono utworow do dodania.');
    }

    const wasIdle = !player.playing && !player.queue.current;
    await player.queue.add(lavalinkTracks);
    if (wasIdle) {
      await player.play();
    }

    return {
      added: lavalinkTracks.length,
      started: wasIdle,
      startedTrack: wasIdle ? normalizeTrack(lavalinkTracks[0]) : null,
      requestedBy,
    };
  }

  getQueueSnapshot(guildId) {
    const player = this.manager.getPlayer(String(guildId || ''));
    if (!player) return null;

    const currentTrack = player.queue.current ? normalizeTrack(player.queue.current) : null;
    const queue = player.queue.tracks.map(normalizeTrack);
    if (!currentTrack && queue.length === 0 && !player.playing) return null;
    const filters = this.getFilterState(player.guildId);

    return {
      guildId: player.guildId,
      voiceChannelId: player.voiceChannelId,
      currentTrack,
      queue,
      playing: player.playing,
      paused: player.paused,
      volume: player.volume,
      positionSec: Math.round(Number(player.position || 0) / 1000),
      repeatMode: normalizeRepeatMode(player.repeatMode),
      muted: Number(player.volume || 0) === 0,
      autoplay: this.getAutoplayEnabled(player.guildId),
      filters: {
        bassboost: filters.bassboost === true,
        nightcore: filters.nightcore === true,
        vaporwave: filters.vaporwave === true,
        eightD: filters.eightD === true,
        karaoke: filters.karaoke === true,
        softLimiter: filters.softLimiter === true,
        eqPreset: filters.eqPreset || 'flat',
      },
    };
  }

  getDiagnostics(guildId = null) {
    const configuredNodes = buildLavalinkNodesFromEnv();
    const runtimeNodes = [...this.manager.nodeManager.nodes.values()].map((node) => ({
      id: String(node?.options?.id || node?.id || 'unknown'),
      host: String(node?.options?.host || ''),
      port: Number(node?.options?.port || 0) || null,
      secure: node?.options?.secure === true,
      connected: node?.connected === true,
      sessionId: node?.sessionId || null,
      reconnectionState: String(node?.reconnectionState || 'IDLE'),
      players: Number(node?.stats?.players || 0) || 0,
      playingPlayers: Number(node?.stats?.playingPlayers || 0) || 0,
      cpuLoad: Number(node?.stats?.cpu?.lavalinkLoad || 0) || 0,
      systemLoad: Number(node?.stats?.cpu?.systemLoad || 0) || 0,
      memoryUsed: Number(node?.stats?.memory?.used || 0) || 0,
    }));

    const normalizedGuildId = guildId ? String(guildId) : null;
    const player = normalizedGuildId
      ? this.manager.getPlayer(normalizedGuildId)
      : null;
    const snapshot = normalizedGuildId
      ? this.getQueueSnapshot(normalizedGuildId)
      : null;

    return {
      initialized: this.initialized === true,
      usable: this.manager.useable === true,
      defaultSearchSource: this.defaultSearchSource,
      recoverySources: [...this.recoverySources],
      defaultVolume: this.defaultVolume,
      idleDisconnectMs: this.idleDisconnectMs,
      configuredNodes: configuredNodes.map((node) => ({
        id: String(node.id || 'unknown'),
        host: String(node.host || ''),
        port: Number(node.port || 0) || null,
        secure: node.secure === true,
      })),
      config: {
        usesUrl: String(process.env.LAVALINK_URL || '').trim().length > 0,
        hasExplicitHost: String(process.env.LAVALINK_HOST || '').trim().length > 0,
        hasExplicitPort: String(process.env.LAVALINK_PORT || '').trim().length > 0,
        hasExplicitPassword: String(process.env.LAVALINK_PASSWORD || '').trim().length > 0,
        hasExplicitSecure: String(process.env.LAVALINK_SECURE || '').trim().length > 0,
        hasExplicitNodeId: String(process.env.LAVALINK_NODE_ID || '').trim().length > 0,
        hasExplicitSearchSource: String(process.env.LAVALINK_SEARCH_SOURCE || '').trim().length > 0,
      },
      nodes: runtimeNodes,
      connectedNodes: runtimeNodes.filter((node) => node.connected).length,
      activePlayers: this.manager.players.size,
      voiceStatusChannels: this.voiceChannelStatuses.size,
      queueStateDir: this.queueStateDir,
      queueStateDirExists: fs.existsSync(this.queueStateDir),
      legacyMusicPresenceCleared: this.legacyMusicPresenceCleared === true,
      guildSession: normalizedGuildId && player
        ? {
          guildId: normalizedGuildId,
          voiceChannelId: String(player.voiceChannelId || snapshot?.voiceChannelId || ''),
          nodeId: String(player?.node?.options?.id || player?.node?.id || ''),
          nodeConnected: player?.node?.connected === true,
          playerConnected: player?.connected === true,
          wsPing: Number(player?.ping?.ws ?? NaN),
          currentTrackTitle: snapshot?.currentTrack?.title || null,
          queueSize: Number(snapshot?.queue?.length || 0) || 0,
          playing: snapshot?.playing === true,
          paused: snapshot?.paused === true,
          statusText: this.getVoiceChannelStatusText(player) || null,
          cachedStatusText: this.voiceChannelStatuses.get(String(player.voiceChannelId || '')) || null,
          hasSavedQueue: this.hasSavedQueueState(normalizedGuildId),
        }
        : null,
    };
  }

  getPlayerForGuild(guildId) {
    const player = this.manager.getPlayer(String(guildId || ''));
    if (!player || (!player.queue.current && player.queue.tracks.length === 0 && !player.playing)) {
      return null;
    }
    return player;
  }

  async pause(guildId) {
    const player = this.getPlayerForGuild(guildId);
    if (!player || !player.queue.current) return null;
    if (!player.paused) {
      await player.pause();
      this.refreshMusicPresence();
    }
    return this.getQueueSnapshot(guildId);
  }

  async resume(guildId) {
    const player = this.getPlayerForGuild(guildId);
    if (!player || !player.queue.current) return null;
    if (player.paused) {
      await player.resume();
      this.refreshMusicPresence();
    }
    return this.getQueueSnapshot(guildId);
  }

  async togglePause(guildId) {
    const player = this.getPlayerForGuild(guildId);
    if (!player || !player.queue.current) return null;

    if (player.paused) {
      await player.resume();
    } else {
      await player.pause();
    }

    this.refreshMusicPresence();
    return this.getQueueSnapshot(guildId);
  }

  async adjustVolume(guildId, delta) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    const current = Number(player.volume || this.defaultVolume);
    if (current > 0) {
      this.previousVolumes.set(String(guildId), current);
    }
    const next = Math.max(0, Math.min(150, current + Number(delta || 0)));
    await player.setVolume(next, true);
    return this.getQueueSnapshot(guildId);
  }

  async toggleMute(guildId) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    const normalizedGuildId = String(guildId);
    const current = Number(player.volume || 0);
    if (current > 0) {
      this.previousVolumes.set(normalizedGuildId, current);
      await player.setVolume(0, true);
    } else {
      const previous = this.previousVolumes.get(normalizedGuildId) || this.defaultVolume || 80;
      await player.setVolume(Math.max(1, Math.min(150, previous)), true);
    }
    return this.getQueueSnapshot(guildId);
  }

  async cycleLoop(guildId) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    const current = normalizeRepeatMode(player.repeatMode);
    const next = current === 'off' ? 'track' : (current === 'track' ? 'queue' : 'off');
    await player.setRepeatMode(next);
    return this.getQueueSnapshot(guildId);
  }

  async toggleBassboost(guildId) {
    const state = this.getFilterState(guildId);
    if (state.bassboost) {
      await this.applyEqPreset(guildId, 'flat');
    } else {
      await this.applyEqPreset(guildId, 'bassboost');
    }
    return this.getQueueSnapshot(guildId);
  }

  async toggleVaporwave(guildId) {
    await this.toggleFx(guildId, 'vaporwave');
    return this.getQueueSnapshot(guildId);
  }

  async applyEqPreset(guildId, presetValue) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    const preset = String(presetValue || '').toLowerCase();
    const state = this.getFilterState(guildId);

    if (preset === 'nightcore') {
      if (!state.nightcore) {
        await player.filterManager.toggleNightcore();
      }
      this.setFilterState(guildId, {
        nightcore: true,
        vaporwave: false,
        eqPreset: 'nightcore',
        bassboost: false,
      });
      return this.getQueueSnapshot(guildId);
    }

    if (state.nightcore) {
      await player.filterManager.toggleNightcore();
      this.setFilterState(guildId, { nightcore: false });
    }

    if (preset === 'flat') {
      await player.filterManager.clearEQ();
      this.setFilterState(guildId, {
        bassboost: false,
        eqPreset: 'flat',
      });
      return this.getQueueSnapshot(guildId);
    }

    if (preset === 'bassboost') {
      await player.filterManager.setEQPreset('BassboostMedium');
      this.setFilterState(guildId, {
        bassboost: true,
        eqPreset: 'bassboost',
      });
      return this.getQueueSnapshot(guildId);
    }

    if (preset === 'vocal' || preset === 'treble') {
      await player.filterManager.setEQ(getEqBandsForPreset(preset));
      this.setFilterState(guildId, {
        bassboost: false,
        eqPreset: preset,
      });
      return this.getQueueSnapshot(guildId);
    }

    return this.getQueueSnapshot(guildId);
  }

  async toggleFx(guildId, fxValue) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    const fx = String(fxValue || '').toLowerCase();
    const state = this.getFilterState(guildId);

    if (fx === 'vaporwave') {
      await player.filterManager.toggleVaporwave();
      const nextVaporwave = !state.vaporwave;
      this.setFilterState(guildId, {
        vaporwave: nextVaporwave,
        nightcore: nextVaporwave ? false : state.nightcore,
      });
      return this.getQueueSnapshot(guildId);
    }

    if (fx === '8d' || fx === 'eightd') {
      await player.filterManager.toggleRotation(0.20);
      this.setFilterState(guildId, { eightD: !state.eightD });
      return this.getQueueSnapshot(guildId);
    }

    if (fx === 'karaoke') {
      await player.filterManager.toggleKaraoke();
      this.setFilterState(guildId, { karaoke: !state.karaoke });
      return this.getQueueSnapshot(guildId);
    }

    if (fx === 'softlimiter') {
      await player.filterManager.toggleLowPass(8);
      this.setFilterState(guildId, { softLimiter: !state.softLimiter });
      return this.getQueueSnapshot(guildId);
    }

    if (fx === 'off') {
      if (state.vaporwave) await player.filterManager.toggleVaporwave();
      if (state.eightD) await player.filterManager.toggleRotation(0.20);
      if (state.karaoke) await player.filterManager.toggleKaraoke();
      if (state.softLimiter) await player.filterManager.toggleLowPass(8);
      this.setFilterState(guildId, {
        vaporwave: false,
        eightD: false,
        karaoke: false,
        softLimiter: false,
      });
      return this.getQueueSnapshot(guildId);
    }

    return this.getQueueSnapshot(guildId);
  }

  async removeFromQueue(guildId, position) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    const numericPosition = Math.floor(Number(position || 0));
    if (!Number.isInteger(numericPosition) || numericPosition <= 0) {
      throw new Error('Pozycja w kolejce musi byc liczba >= 1.');
    }

    const removedResult = await player.queue.remove(numericPosition - 1);
    const removedTrack = Array.isArray(removedResult?.removed)
      ? removedResult.removed[0]
      : removedResult?.removed;

    return {
      position: numericPosition,
      removed: removedTrack ? normalizeTrack(removedTrack) : null,
      snapshot: this.getQueueSnapshot(guildId),
    };
  }

  async clearQueue(guildId) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    const queuedTracks = Array.isArray(player.queue?.tracks) ? player.queue.tracks.length : 0;
    if (queuedTracks > 0) {
      await player.queue.splice(0, queuedTracks);
    }

    return {
      removed: queuedTracks,
      snapshot: this.getQueueSnapshot(guildId),
    };
  }

  async waitForNodeConnection(node, timeoutMs = 3000) {
    if (!node) return false;
    if (node.connected === true) return true;

    const startedAt = Date.now();
    while (Date.now() - startedAt < timeoutMs) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (node.connected === true) {
        return true;
      }
    }

    return node.connected === true;
  }

  async reconnectSession(guildId) {
    const normalizedGuildId = String(guildId || '');
    const player = this.manager.getPlayer(normalizedGuildId);
    if (!player || (!player.queue.current && player.queue.tracks.length === 0 && !player.playing && !player.paused)) {
      return null;
    }

    const targetVoiceChannelId = String(player.options?.voiceChannelId || player.voiceChannelId || '').trim();
    if (!targetVoiceChannelId) {
      throw new Error('Brak zapisanego kanalu glosowego dla tej sesji.');
    }

    const node = player.node || null;
    const nodeWasConnected = node?.connected === true;
    if (node && node.connected !== true) {
      if (typeof node.reconnect === 'function') {
        node.reconnect(true);
      } else if (typeof node.connect === 'function') {
        node.connect();
      }

      const nodeRecovered = await this.waitForNodeConnection(node, 3000);
      if (!nodeRecovered) {
        throw new Error('Lavalink node nadal jest offline po probie reconnect.');
      }
    }

    player.options.voiceChannelId = targetVoiceChannelId;
    if (player.voiceChannelId) {
      await player.disconnect(true).catch(() => {});
    }
    await player.connect();

    this.clearIdleTimer(normalizedGuildId);
    this.refreshMusicPresence();

    return {
      snapshot: this.getQueueSnapshot(normalizedGuildId),
      voiceChannelId: targetVoiceChannelId,
      nodeWasConnected,
      nodeConnected: node?.connected === true,
    };
  }

  saveQueueState(guildId) {
    const snapshot = this.getQueueSnapshot(guildId);
    if (!snapshot || !snapshot.currentTrack) {
      throw new Error('Brak aktywnej kolejki do zapisania.');
    }

    const tracks = [snapshot.currentTrack, ...snapshot.queue]
      .map((track) => ({
        url: String(track?.url || ''),
        title: String(track?.title || ''),
      }))
      .filter((track) => Boolean(track.url));

    if (tracks.length === 0) {
      throw new Error('Brak utworow z linkiem do zapisania.');
    }

    this.ensureQueueStateDir();
    const payload = {
      guildId: String(snapshot.guildId),
      voiceChannelId: String(snapshot.voiceChannelId || ''),
      savedAt: Date.now(),
      volume: Number(snapshot.volume || this.defaultVolume),
      repeatMode: normalizeRepeatMode(snapshot.repeatMode),
      autoplay: snapshot.autoplay === true,
      tracks,
    };

    const filePath = this.getQueueStatePath(snapshot.guildId);
    writeJsonFileAtomic(filePath, payload);
    return {
      path: filePath,
      tracksSaved: tracks.length,
    };
  }

  hasSavedQueueState(guildId) {
    const filePath = this.getQueueStatePath(guildId);
    return fs.existsSync(filePath);
  }

  loadSavedQueueState(guildId) {
    const filePath = this.getQueueStatePath(guildId);
    if (!fs.existsSync(filePath)) {
      throw new Error('Brak zapisanego stanu kolejki.');
    }
    const raw = fs.readFileSync(filePath, 'utf8');
    const parsed = JSON.parse(raw);
    const tracks = Array.isArray(parsed?.tracks)
      ? parsed.tracks.filter((entry) => entry && typeof entry.url === 'string' && entry.url.trim())
      : [];
    if (tracks.length === 0) {
      throw new Error('Zapisana kolejka jest pusta.');
    }
    return {
      ...parsed,
      tracks,
    };
  }

  async restoreQueueState(guild, voiceChannel) {
    const state = this.loadSavedQueueState(guild.id);
    const session = await this.ensureSession(guild, voiceChannel);
    const player = session.player;

    const restoredTracks = [];
    for (const entry of state.tracks) {
      try {
        const resolved = await this.resolveTracks(entry.url, null, player);
        const track = resolved?.tracks?.[0];
        if (!track) continue;
        restoredTracks.push(getTrackFromPublicTrack(track));
      } catch (error) {
        this.logger?.warn?.(`Restore queue skip guild=${guild.id} url="${entry.url}" error=${error.message}`);
      }
    }

    if (restoredTracks.length === 0) {
      throw new Error('Nie udalo sie odtworzyc zadnego utworu z zapisu.');
    }

    const wasIdle = !player.playing && !player.queue.current;
    await player.queue.add(restoredTracks);
    if (wasIdle) {
      await player.play();
    }

    const normalizedVolume = Math.max(1, Math.min(150, Number(state.volume || this.defaultVolume)));
    await player.setVolume(normalizedVolume, true);
    await player.setRepeatMode(normalizeRepeatMode(state.repeatMode));
    this.setAutoplayEnabled(guild.id, state.autoplay === true);

    return {
      restored: restoredTracks.length,
      started: wasIdle,
      snapshot: this.getQueueSnapshot(guild.id),
    };
  }

  async shuffle(guildId) {
    const player = this.getPlayerForGuild(guildId);
    if (!player) return null;

    if (player.queue.tracks.length > 1) {
      await player.queue.shuffle();
    }

    return this.getQueueSnapshot(guildId);
  }

  async skip(guildId) {
    const player = this.manager.getPlayer(String(guildId || ''));
    if (!player || !player.queue.current) return null;

    const skipped = normalizeTrack(player.queue.current);
    await player.skip(0, false).catch(async () => {
      await player.stopPlaying(true, false).catch(() => {});
    });
    this.refreshMusicPresence();
    return skipped;
  }

  async stop(guildId) {
    const id = String(guildId || '');
    this.cancelSpotifyBackfill(id, 'music stop');
    this.clearIdleTimer(id);
    this.previousVolumes.delete(id);
    this.clearFilterState(id);
    this.autoplayStates.delete(id);
    const player = this.manager.getPlayer(id);
    if (!player) {
      this.refreshMusicPresence();
      return false;
    }
    await this.clearVoiceChannelStatus(player.voiceChannelId).catch(() => {});
    player.destroy('music stop', true).catch((error) => {
      this.logger?.warn?.(`Music stop error guild=${id}: ${error.message}`);
    });
    this.refreshMusicPresence();
    return true;
  }

  shutdown() {
    for (const guildId of this.idleTimers.keys()) {
      this.clearIdleTimer(guildId);
    }
    this.previousVolumes.clear();
    this.filterStates.clear();
    this.autoplayStates.clear();
    for (const guildId of this.spotifyBackfillJobs.keys()) {
      this.cancelSpotifyBackfill(guildId, 'shutdown');
    }
    for (const player of this.manager.players.values()) {
      player.destroy('bot shutdown', true).catch(() => {});
    }
    for (const node of this.manager.nodeManager.nodes.values()) {
      node.disconnect('bot shutdown');
    }
    this.refreshMusicPresence();
  }

  scheduleIdleDestroy(guildId) {
    this.clearIdleTimer(guildId);
    const timer = setTimeout(() => {
      const player = this.manager.getPlayer(guildId);
      if (player && !player.playing && !player.queue.current && player.queue.tracks.length === 0) {
        player.destroy('empty queue idle timeout', true).catch(() => {});
      }
      this.idleTimers.delete(guildId);
    }, this.idleDisconnectMs);
    timer.unref?.();
    this.idleTimers.set(guildId, timer);
  }

  clearIdleTimer(guildId) {
    const timer = this.idleTimers.get(guildId);
    if (!timer) return;
    clearTimeout(timer);
    this.idleTimers.delete(guildId);
  }
}

module.exports = {
  MusicManager,
};
