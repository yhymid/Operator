'use strict';

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

function loadSqliteModule() {
  try {
    return require('node:sqlite');
  } catch (error) {
    const hint = 'SQLite lab wymaga `node:sqlite` (Node >=22.13 bez flagi albo Node 22.5-22.12 z `--experimental-sqlite`).';
    error.message = `${hint} ${error.message}`;
    throw error;
  }
}

function ensureDir(dirPath) {
  fs.mkdirSync(dirPath, { recursive: true });
}

function listJsonFiles(dirPath) {
  if (!dirPath || !fs.existsSync(dirPath)) return [];
  return fs.readdirSync(dirPath, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith('.json'))
    .map((entry) => path.join(dirPath, entry.name))
    .sort((a, b) => a.localeCompare(b));
}

function walkFiles(rootDir) {
  if (!rootDir || !fs.existsSync(rootDir)) return [];

  const results = [];
  const stack = [rootDir];
  while (stack.length > 0) {
    const current = stack.pop();
    const entries = fs.readdirSync(current, { withFileTypes: true })
      .sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      const fullPath = path.join(current, entry.name);
      if (entry.isDirectory()) {
        stack.push(fullPath);
      } else if (entry.isFile()) {
        results.push(fullPath);
      }
    }
  }

  return results.sort((a, b) => a.localeCompare(b));
}

function readJsonFile(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function stableClone(value) {
  if (Array.isArray(value)) {
    return value.map((entry) => stableClone(entry));
  }
  if (!value || typeof value !== 'object') {
    return value;
  }

  const next = {};
  for (const key of Object.keys(value).sort((a, b) => a.localeCompare(b))) {
    next[key] = stableClone(value[key]);
  }
  return next;
}

function stableStringify(value) {
  return JSON.stringify(stableClone(value));
}

function collectServerState(serverCacheDir) {
  const guilds = {};
  for (const filePath of listJsonFiles(serverCacheDir)) {
    const guildId = path.basename(filePath, '.json');
    guilds[guildId] = readJsonFile(filePath);
  }
  return guilds;
}

function collectMusicQueueState(musicQueueDir) {
  const queues = {};
  for (const filePath of listJsonFiles(musicQueueDir)) {
    const guildId = path.basename(filePath, '.json');
    queues[guildId] = readJsonFile(filePath);
  }
  return queues;
}

function collectUserCommandState(userCacheDir) {
  const users = {};
  for (const filePath of walkFiles(userCacheDir)) {
    if (path.basename(filePath) !== 'commands.json') continue;
    const userId = path.basename(path.dirname(filePath));
    users[userId] = readJsonFile(filePath);
  }
  return users;
}

function buildStateSnapshot({
  serverCacheDir,
  musicQueueDir,
  userCacheDir,
  generatedAt = new Date().toISOString(),
} = {}) {
  return {
    generatedAt,
    sources: {
      serverCacheDir: serverCacheDir || null,
      musicQueueDir: musicQueueDir || null,
      userCacheDir: userCacheDir || null,
    },
    guilds: collectServerState(serverCacheDir),
    musicQueues: collectMusicQueueState(musicQueueDir),
    userCommands: collectUserCommandState(userCacheDir),
  };
}

function hashFile(filePath) {
  const hash = crypto.createHash('sha256');
  hash.update(fs.readFileSync(filePath));
  return hash.digest('hex');
}

function copyDirIfExists(sourceDir, targetDir) {
  if (!sourceDir || !fs.existsSync(sourceDir)) return false;
  ensureDir(path.dirname(targetDir));
  fs.cpSync(sourceDir, targetDir, { recursive: true });
  return true;
}

function createStateBackup({
  serverCacheDir,
  musicQueueDir,
  userCacheDir,
  backupDir,
  createdAt = new Date().toISOString(),
} = {}) {
  if (!backupDir) {
    throw new Error('backupDir is required');
  }

  ensureDir(backupDir);

  const targets = {
    server: path.join(backupDir, 'server'),
    musicQueue: path.join(backupDir, 'music-queue'),
    user: path.join(backupDir, 'user'),
  };

  copyDirIfExists(serverCacheDir, targets.server);
  copyDirIfExists(musicQueueDir, targets.musicQueue);
  copyDirIfExists(userCacheDir, targets.user);

  const manifest = {
    createdAt,
    sources: {
      serverCacheDir: serverCacheDir || null,
      musicQueueDir: musicQueueDir || null,
      userCacheDir: userCacheDir || null,
    },
    backupTargets: targets,
    files: [],
  };

  for (const [scope, targetDir] of Object.entries(targets)) {
    for (const filePath of walkFiles(targetDir)) {
      const relativePath = path.relative(backupDir, filePath);
      const stats = fs.statSync(filePath);
      manifest.files.push({
        scope,
        path: relativePath,
        size: stats.size,
        sha256: hashFile(filePath),
      });
    }
  }

  manifest.files.sort((a, b) => a.path.localeCompare(b.path));
  fs.writeFileSync(path.join(backupDir, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  return manifest;
}

function restoreStateBackup({
  backupDir,
  serverCacheDir,
  musicQueueDir,
  userCacheDir,
} = {}) {
  if (!backupDir) {
    throw new Error('backupDir is required');
  }

  const sources = {
    server: {
      from: path.join(backupDir, 'server'),
      to: serverCacheDir,
    },
    musicQueue: {
      from: path.join(backupDir, 'music-queue'),
      to: musicQueueDir,
    },
    user: {
      from: path.join(backupDir, 'user'),
      to: userCacheDir,
    },
  };

  for (const entry of Object.values(sources)) {
    if (!entry.to) continue;
    fs.rmSync(entry.to, { recursive: true, force: true });
    if (fs.existsSync(entry.from)) {
      fs.cpSync(entry.from, entry.to, { recursive: true });
    } else {
      ensureDir(entry.to);
    }
  }
}

function initializeStateLabSchema(db) {
  db.exec(`
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = FULL;
    PRAGMA foreign_keys = ON;

    CREATE TABLE IF NOT EXISTS migration_meta (
      key TEXT PRIMARY KEY,
      value_json TEXT NOT NULL,
      updated_at TEXT NOT NULL
    ) STRICT;

    CREATE TABLE IF NOT EXISTS guild_state_documents (
      guild_id TEXT PRIMARY KEY,
      guild_name TEXT,
      version INTEGER NOT NULL,
      payload_json TEXT NOT NULL,
      updated_at TEXT NOT NULL
    ) STRICT;

    CREATE TABLE IF NOT EXISTS music_queue_documents (
      guild_id TEXT PRIMARY KEY,
      payload_json TEXT NOT NULL,
      updated_at TEXT NOT NULL
    ) STRICT;

    CREATE TABLE IF NOT EXISTS user_command_documents (
      user_id TEXT PRIMARY KEY,
      payload_json TEXT NOT NULL,
      updated_at TEXT NOT NULL
    ) STRICT;
  `);
}

function resetStateLabTables(db) {
  db.exec(`
    DELETE FROM migration_meta;
    DELETE FROM guild_state_documents;
    DELETE FROM music_queue_documents;
    DELETE FROM user_command_documents;
  `);
}

function writeStateSnapshotToSqlite({
  dbPath,
  snapshot,
  meta = {},
} = {}) {
  if (!dbPath) {
    throw new Error('dbPath is required');
  }
  if (!snapshot || typeof snapshot !== 'object') {
    throw new Error('snapshot is required');
  }

  ensureDir(path.dirname(dbPath));
  const { DatabaseSync } = loadSqliteModule();
  const db = new DatabaseSync(dbPath);

  try {
    initializeStateLabSchema(db);
    resetStateLabTables(db);

    const insertMeta = db.prepare(`
      INSERT INTO migration_meta (key, value_json, updated_at)
      VALUES (?, ?, ?)
    `);
    const insertGuild = db.prepare(`
      INSERT INTO guild_state_documents (guild_id, guild_name, version, payload_json, updated_at)
      VALUES (?, ?, ?, ?, ?)
    `);
    const insertQueue = db.prepare(`
      INSERT INTO music_queue_documents (guild_id, payload_json, updated_at)
      VALUES (?, ?, ?)
    `);
    const insertUser = db.prepare(`
      INSERT INTO user_command_documents (user_id, payload_json, updated_at)
      VALUES (?, ?, ?)
    `);

    const nowIso = new Date().toISOString();
    db.exec('BEGIN IMMEDIATE');
    try {
      insertMeta.run('schema_version', JSON.stringify({ version: 1 }), nowIso);
      insertMeta.run('snapshot_meta', JSON.stringify({
        generatedAt: snapshot.generatedAt || nowIso,
        sources: snapshot.sources || {},
        ...meta,
      }), nowIso);

      for (const [guildId, payload] of Object.entries(snapshot.guilds || {}).sort(([a], [b]) => a.localeCompare(b))) {
        insertGuild.run(
          String(guildId),
          payload?.guildName || null,
          Number(payload?.version || 1),
          JSON.stringify(payload),
          nowIso,
        );
      }

      for (const [guildId, payload] of Object.entries(snapshot.musicQueues || {}).sort(([a], [b]) => a.localeCompare(b))) {
        insertQueue.run(String(guildId), JSON.stringify(payload), nowIso);
      }

      for (const [userId, payload] of Object.entries(snapshot.userCommands || {}).sort(([a], [b]) => a.localeCompare(b))) {
        insertUser.run(String(userId), JSON.stringify(payload), nowIso);
      }

      db.exec('COMMIT');
    } catch (error) {
      db.exec('ROLLBACK');
      throw error;
    }

    return {
      dbPath,
      guilds: Object.keys(snapshot.guilds || {}).length,
      musicQueues: Object.keys(snapshot.musicQueues || {}).length,
      userCommands: Object.keys(snapshot.userCommands || {}).length,
    };
  } finally {
    db.close();
  }
}

function readStateSnapshotFromSqlite({ dbPath } = {}) {
  if (!dbPath) {
    throw new Error('dbPath is required');
  }

  const { DatabaseSync } = loadSqliteModule();
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const metaRow = db.prepare(`
      SELECT value_json
      FROM migration_meta
      WHERE key = ?
    `).get('snapshot_meta');

    const snapshot = {
      generatedAt: metaRow ? JSON.parse(metaRow.value_json).generatedAt || null : null,
      sources: metaRow ? JSON.parse(metaRow.value_json).sources || {} : {},
      guilds: {},
      musicQueues: {},
      userCommands: {},
    };

    for (const row of db.prepare(`
      SELECT guild_id, payload_json
      FROM guild_state_documents
      ORDER BY guild_id
    `).iterate()) {
      snapshot.guilds[String(row.guild_id)] = JSON.parse(row.payload_json);
    }

    for (const row of db.prepare(`
      SELECT guild_id, payload_json
      FROM music_queue_documents
      ORDER BY guild_id
    `).iterate()) {
      snapshot.musicQueues[String(row.guild_id)] = JSON.parse(row.payload_json);
    }

    for (const row of db.prepare(`
      SELECT user_id, payload_json
      FROM user_command_documents
      ORDER BY user_id
    `).iterate()) {
      snapshot.userCommands[String(row.user_id)] = JSON.parse(row.payload_json);
    }

    return snapshot;
  } finally {
    db.close();
  }
}

function compareStateSnapshots(expected, actual) {
  const left = stableClone(expected);
  const right = stableClone(actual);

  if (stableStringify(left) === stableStringify(right)) {
    return {
      equal: true,
      differences: [],
    };
  }

  const differences = [];
  const maxDifferences = 25;

  function visit(expectedValue, actualValue, currentPath) {
    if (differences.length >= maxDifferences) return;

    const expectedIsArray = Array.isArray(expectedValue);
    const actualIsArray = Array.isArray(actualValue);
    if (expectedIsArray || actualIsArray) {
      if (!expectedIsArray || !actualIsArray) {
        differences.push(`${currentPath}: type mismatch`);
        return;
      }
      if (expectedValue.length !== actualValue.length) {
        differences.push(`${currentPath}: array length ${expectedValue.length} != ${actualValue.length}`);
      }
      const length = Math.max(expectedValue.length, actualValue.length);
      for (let index = 0; index < length; index += 1) {
        visit(expectedValue[index], actualValue[index], `${currentPath}[${index}]`);
      }
      return;
    }

    const expectedIsObject = Boolean(expectedValue && typeof expectedValue === 'object');
    const actualIsObject = Boolean(actualValue && typeof actualValue === 'object');
    if (expectedIsObject || actualIsObject) {
      if (!expectedIsObject || !actualIsObject) {
        differences.push(`${currentPath}: type mismatch`);
        return;
      }
      const keys = [...new Set([
        ...Object.keys(expectedValue),
        ...Object.keys(actualValue),
      ])].sort((a, b) => a.localeCompare(b));
      for (const key of keys) {
        visit(expectedValue[key], actualValue[key], currentPath ? `${currentPath}.${key}` : key);
      }
      return;
    }

    if (expectedValue !== actualValue) {
      differences.push(`${currentPath}: ${JSON.stringify(expectedValue)} != ${JSON.stringify(actualValue)}`);
    }
  }

  visit(left, right, '');
  return {
    equal: false,
    differences,
  };
}

module.exports = {
  buildStateSnapshot,
  compareStateSnapshots,
  createStateBackup,
  readStateSnapshotFromSqlite,
  restoreStateBackup,
  stableClone,
  stableStringify,
  writeStateSnapshotToSqlite,
};
