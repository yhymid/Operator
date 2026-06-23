'use strict';

require('dotenv').config({ quiet: true });

const fs = require('fs');

const { readOperatorConfigSync, formatConfigError } = require('../src/core/config');
const { resolveOwnerIds } = require('../src/core/owners');
const { resolveOperatorPaths } = require('../src/core/paths');

const TARGET_NODE_MAJOR = 22;

function ensureWritableDir(dirPath) {
  fs.mkdirSync(dirPath, { recursive: true });
  fs.accessSync(dirPath, fs.constants.W_OK);
}

function main() {
  const paths = resolveOperatorPaths();
  const warnings = [];
  const errors = [];

  const nodeVersion = process.versions.node;
  const nodeMajor = Number(nodeVersion.split('.')[0]);
  if (nodeMajor !== TARGET_NODE_MAJOR) {
    warnings.push(`Runtime jest na Node ${nodeVersion}, a projekt celuje w Node ${TARGET_NODE_MAJOR}.x.`);
  }

  if (!process.env.BOT_TOKEN) {
    errors.push('Brak BOT_TOKEN w env.');
  }

  try {
    readOperatorConfigSync(paths.configPath);
  } catch (error) {
    errors.push(formatConfigError(error, { prefix: 'Config: ' }));
  }

  try {
    ensureWritableDir(paths.dataDir);
    ensureWritableDir(paths.logsDir);
    ensureWritableDir(paths.serverCacheDir);
    ensureWritableDir(paths.musicQueueDir);
    ensureWritableDir(paths.userCacheDir);
    ensureWritableDir(paths.serverLogsDir);
  } catch (error) {
    errors.push(`Katalogi runtime nie sa gotowe do zapisu: ${error.message}`);
  }

  const ownerAccess = resolveOwnerIds({ configPath: paths.configPath });
  if (ownerAccess.ids.size === 0) {
    warnings.push('Brak OWNER_IDS. Komendy owner-only beda niedostepne.');
  }
  if (ownerAccess.invalid.length > 0) {
    warnings.push(`Pominiete niepoprawne OWNER_IDS: ${ownerAccess.invalid.join(', ')}`);
  }

  const lines = [
    'Operator preflight',
    `- Node.js: ${nodeVersion}`,
    `- Config: ${paths.configPath}`,
    `- Data dir: ${paths.dataDir}`,
    `- Logs dir: ${paths.logsDir}`,
    `- Sync komend na starcie: ${paths.syncCommandsOnStartup ? 'tak' : 'nie'}`,
  ];

  if (warnings.length > 0) {
    lines.push('');
    lines.push('Ostrzezenia:');
    for (const warning of warnings) {
      lines.push(`- ${warning}`);
    }
  }

  if (errors.length > 0) {
    lines.push('');
    lines.push('Bledy:');
    for (const error of errors) {
      lines.push(`- ${error}`);
    }
  }

  console.log(lines.join('\n'));

  if (errors.length > 0) {
    process.exitCode = 1;
  }
}

main();
