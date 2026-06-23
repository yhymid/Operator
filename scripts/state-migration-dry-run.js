'use strict';

const fs = require('fs');
const path = require('path');

const { PROJECT_ROOT, resolveOperatorPaths } = require('../src/core/paths');
const {
  buildStateSnapshot,
  compareStateSnapshots,
  createStateBackup,
  readStateSnapshotFromSqlite,
  writeStateSnapshotToSqlite,
} = require('../src/core/state-migration');

function parseArgs(argv) {
  const result = {};
  for (const entry of argv) {
    if (!entry.startsWith('--')) continue;
    const [rawKey, ...rest] = entry.slice(2).split('=');
    const key = rawKey.trim();
    const value = rest.length > 0 ? rest.join('=').trim() : 'true';
    result[key] = value;
  }
  return result;
}

function formatTimestamp(date = new Date()) {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  const hours = String(date.getHours()).padStart(2, '0');
  const minutes = String(date.getMinutes()).padStart(2, '0');
  const seconds = String(date.getSeconds()).padStart(2, '0');
  return `${year}${month}${day}-${hours}${minutes}${seconds}`;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const runtimePaths = resolveOperatorPaths(process.env);
  const projectRoot = path.resolve(args['project-root'] || PROJECT_ROOT);
  const backupBaseDir = path.resolve(args['backup-dir'] || path.join(projectRoot, 'backups', 'state'));
  const dbBaseDir = path.resolve(args['db-dir'] || path.join(projectRoot, 'cache', 'state-lab'));
  const stamp = formatTimestamp();

  const serverCacheDir = path.resolve(args['server-cache-dir'] || runtimePaths.serverCacheDir);
  const musicQueueDir = path.resolve(args['music-queue-dir'] || runtimePaths.musicQueueDir);
  const userCacheDir = path.resolve(args['user-cache-dir'] || runtimePaths.userCacheDir);
  const backupDir = path.join(backupBaseDir, `json-backup-${stamp}`);
  const dbPath = path.join(dbBaseDir, `state-lab-${stamp}.sqlite`);

  fs.mkdirSync(backupBaseDir, { recursive: true });
  fs.mkdirSync(dbBaseDir, { recursive: true });

  const manifest = createStateBackup({
    serverCacheDir,
    musicQueueDir,
    userCacheDir,
    backupDir,
  });

  const sourceSnapshot = buildStateSnapshot({
    serverCacheDir,
    musicQueueDir,
    userCacheDir,
  });

  const writeResult = writeStateSnapshotToSqlite({
    dbPath,
    snapshot: sourceSnapshot,
    meta: {
      projectRoot,
      backupDir,
      dryRun: true,
    },
  });

  const sqliteSnapshot = readStateSnapshotFromSqlite({ dbPath });
  const comparison = compareStateSnapshots(sourceSnapshot, sqliteSnapshot);

  if (!comparison.equal) {
    console.error('Dry-run migration mismatch detected.');
    for (const difference of comparison.differences) {
      console.error(`- ${difference}`);
    }
    process.exitCode = 1;
    return;
  }

  console.log('SQLite dry-run migration OK');
  console.log(`Project root: ${projectRoot}`);
  console.log(`Backup dir: ${backupDir}`);
  console.log(`SQLite lab DB: ${dbPath}`);
  console.log(`Backed up files: ${manifest.files.length}`);
  console.log(`Guild docs: ${writeResult.guilds}`);
  console.log(`Music queue docs: ${writeResult.musicQueues}`);
  console.log(`User command docs: ${writeResult.userCommands}`);
}

main();
