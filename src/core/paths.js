const path = require('path');

const PROJECT_ROOT = path.join(__dirname, '..', '..');

function envBoolean(name, fallback = false, env = process.env) {
  const raw = String(env?.[name] || '').trim();
  if (!raw) return fallback;
  return /^(1|true|yes|on)$/i.test(raw);
}

function resolveProjectPath(rawValue, fallbackRelativePath) {
  const normalized = String(rawValue || '').trim();
  if (!normalized) {
    return path.join(PROJECT_ROOT, fallbackRelativePath);
  }
  return path.isAbsolute(normalized)
    ? normalized
    : path.join(PROJECT_ROOT, normalized);
}

function resolveOperatorPaths(env = process.env) {
  const configPath = resolveProjectPath(env.OPERATOR_CONFIG_PATH, 'config.json');
  const dataDir = resolveProjectPath(env.OPERATOR_DATA_DIR, 'cache');
  const logsDir = resolveProjectPath(env.OPERATOR_LOG_DIR, 'logs');

  return {
    projectRoot: PROJECT_ROOT,
    configPath,
    dataDir,
    logsDir,
    serverCacheDir: path.join(dataDir, 'server'),
    musicQueueDir: path.join(dataDir, 'music-queue'),
    userCacheDir: path.join(dataDir, 'user'),
    serverLogsDir: path.join(logsDir, 'server'),
    syncCommandsOnStartup: envBoolean('OPERATOR_SYNC_COMMANDS_ON_STARTUP', true, env),
  };
}

module.exports = {
  PROJECT_ROOT,
  envBoolean,
  resolveOperatorPaths,
};
