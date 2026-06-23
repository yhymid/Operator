'use strict';

require('dotenv').config({ quiet: true });

const {
  Client,
  GatewayIntentBits,
} = require('discord.js');

const {
  formatCommandSyncReport,
  syncGlobalCommands,
  syncGuildCommands,
} = require('../src/core/command-sync');

function parseArgs(argv) {
  const result = {
    guildIds: [],
    global: false,
    clearGlobal: false,
    help: false,
  };

  for (const entry of argv) {
    if (entry === '--global') {
      result.global = true;
      continue;
    }
    if (entry === '--clear-global') {
      result.clearGlobal = true;
      continue;
    }
    if (entry === '--help' || entry === '-h') {
      result.help = true;
      continue;
    }
    if (entry.startsWith('--guild=')) {
      const raw = entry.slice('--guild='.length);
      result.guildIds.push(...raw.split(',').map((item) => item.trim()).filter(Boolean));
    }
  }

  result.guildIds = [...new Set(result.guildIds)];
  return result;
}

function printUsage() {
  console.log([
    'Uzycie:',
    '  node ./scripts/register-commands.js',
    '  node ./scripts/register-commands.js --guild=123456789012345678',
    '  node ./scripts/register-commands.js --guild=123,456',
    '  node ./scripts/register-commands.js --global',
    '  node ./scripts/register-commands.js --clear-global',
  ].join('\n'));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));

  if (args.help) {
    printUsage();
    return;
  }

  if (!process.env.BOT_TOKEN) {
    throw new Error('Brak BOT_TOKEN w env.');
  }

  const client = new Client({
    intents: [GatewayIntentBits.Guilds],
  });

  client.once('clientReady', async () => {
    try {
      let report;
      if (args.clearGlobal) {
        report = await syncGlobalCommands(client, { clear: true });
      } else if (args.global) {
        report = await syncGlobalCommands(client);
      } else {
        report = await syncGuildCommands(client, {
          guildIds: args.guildIds,
        });
      }

      console.log(formatCommandSyncReport(report).join('\n'));
      await client.destroy();
    } catch (error) {
      console.error(`Sync komend nieudany: ${error.message}`);
      await client.destroy();
      process.exitCode = 1;
    }
  });

  client.on('error', (error) => {
    console.error(`Discord client error: ${error.message}`);
  });

  await client.login(process.env.BOT_TOKEN);
}

main().catch((error) => {
  console.error(error.message || error);
  process.exit(1);
});
