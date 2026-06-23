const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const projectRoot = path.join(__dirname, '..');
const includeEntries = ['main.js', 'src', 'scripts'];
const excludeDirs = new Set(['node_modules', '.git', 'cache', 'logs', 'var', 'backups', '.local-lavalink', 'plugins']);

function collectJsFiles(entryPath) {
  if (!fs.existsSync(entryPath)) {
    return [];
  }

  const stats = fs.statSync(entryPath);
  if (stats.isFile()) {
    return entryPath.endsWith('.js') ? [entryPath] : [];
  }

  if (!stats.isDirectory()) {
    return [];
  }

  const files = [];
  for (const entry of fs.readdirSync(entryPath, { withFileTypes: true })) {
    if (entry.isDirectory() && excludeDirs.has(entry.name)) {
      continue;
    }
    files.push(...collectJsFiles(path.join(entryPath, entry.name)));
  }
  return files;
}

const files = includeEntries
  .flatMap((entry) => collectJsFiles(path.join(projectRoot, entry)))
  .filter((filePath) => path.basename(filePath) !== path.basename(__filename))
  .sort();

if (files.length === 0) {
  console.error('No JavaScript files found for syntax check.');
  process.exit(1);
}

for (const filePath of files) {
  const result = spawnSync(process.execPath, ['--check', filePath], {
    cwd: projectRoot,
    encoding: 'utf8',
  });

  if (result.status !== 0) {
    process.stderr.write(result.stdout || '');
    process.stderr.write(result.stderr || '');
    console.error(`Syntax check failed: ${path.relative(projectRoot, filePath)}`);
    process.exit(result.status || 1);
  }
}

console.log(`Syntax OK: ${files.length} files`);
