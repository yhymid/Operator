const { readOperatorConfigSync } = require('./config');
const { resolveOperatorPaths } = require('./paths');

const DEFAULT_CONFIG_PATH = resolveOperatorPaths().configPath;
const SNOWFLAKE_PATTERN = /^\d{16,22}$/;

function normalizeOwnerId(value) {
  const normalized = String(value || '').trim();
  if (!normalized) return null;
  return SNOWFLAKE_PATTERN.test(normalized) ? normalized : null;
}

function parseOwnerIdsInput(value) {
  if (Array.isArray(value)) {
    const ids = [];
    const invalid = [];

    for (const entry of value) {
      const normalized = normalizeOwnerId(entry);
      if (normalized) {
        ids.push(normalized);
      } else if (String(entry || '').trim()) {
        invalid.push(String(entry).trim());
      }
    }

    return {
      ids: [...new Set(ids)],
      invalid,
    };
  }

  if (value === undefined || value === null) {
    return { ids: [], invalid: [] };
  }

  const tokens = String(value)
    .split(/[,\s;]+/g)
    .map((entry) => entry.trim())
    .filter(Boolean);

  const ids = [];
  const invalid = [];

  for (const token of tokens) {
    const normalized = normalizeOwnerId(token);
    if (normalized) {
      ids.push(normalized);
    } else {
      invalid.push(token);
    }
  }

  return {
    ids: [...new Set(ids)],
    invalid,
  };
}

function readOwnerIdsFromConfig(configPath = DEFAULT_CONFIG_PATH) {
  try {
    const config = readOperatorConfigSync(configPath, { allowMissing: true });
    if (!config || !Array.isArray(config.ownerIds)) {
      return {
        ids: [],
        invalid: [],
      };
    }
    return parseOwnerIdsInput(config.ownerIds);
  } catch {
    return {
      ids: [],
      invalid: [],
    };
  }
}

function resolveOwnerIds(options = {}) {
  const env = options.env || process.env;
  const configPath = options.configPath || DEFAULT_CONFIG_PATH;
  const envCandidates = [
    ['OWNER_IDS', env.OWNER_IDS],
    ['OWNER_ID', env.OWNER_ID],
  ];
  const invalid = [];

  for (const [key, rawValue] of envCandidates) {
    const parsed = parseOwnerIdsInput(rawValue);
    invalid.push(...parsed.invalid);
    if (parsed.ids.length > 0) {
      return {
        ids: new Set(parsed.ids),
        invalid,
        source: `env:${key}`,
      };
    }
  }

  const configOwners = readOwnerIdsFromConfig(configPath);
  invalid.push(...configOwners.invalid);
  if (configOwners.ids.length > 0) {
    return {
      ids: new Set(configOwners.ids),
      invalid,
      source: 'config:ownerIds',
    };
  }

  return {
    ids: new Set(),
    invalid,
    source: 'fallback-empty',
  };
}

function getOwnerIds(options = {}) {
  return resolveOwnerIds(options).ids;
}

function isOwner(userId, options = {}) {
  return getOwnerIds(options).has(String(userId || '').trim());
}

const exported = {
  getOwnerIds,
  isOwner,
  parseOwnerIdsInput,
  resolveOwnerIds,
};

Object.defineProperty(exported, 'OWNER_IDS', {
  enumerable: true,
  get() {
    return getOwnerIds();
  },
});

module.exports = exported;
