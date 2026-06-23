const { announceCommand } = require('../cmds/announce');
const { automodCommand } = require('../cmds/automod');
const { cacheCommand } = require('../cmds/cache');
const { cleanCommand } = require('../cmds/clean');
const { debugCommand, healthCommand } = require('../cmds/diagnostics');
const { dmCommand } = require('../cmds/dm');
const { helpCommand } = require('../cmds/help-command');
const { logsCommand } = require('../cmds/logs');
const { musicCommand } = require('../cmds/music');
const { settingsCommand } = require('../cmds/settings');
const { verifyCommand } = require('../cmds/verify');

function getApplicationCommandPayload() {
  return [
    helpCommand,
    settingsCommand,
    cleanCommand,
    cacheCommand,
    musicCommand,
    verifyCommand,
    automodCommand,
    announceCommand,
    logsCommand,
    healthCommand,
    debugCommand,
    dmCommand,
  ];
}

module.exports = {
  getApplicationCommandPayload,
};
