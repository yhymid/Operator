const { getApplicationCommandPayload } = require('./command-manifest');

async function resolveTargetGuilds(client, guildIds = []) {
  if (!Array.isArray(guildIds) || guildIds.length === 0) {
    return {
      guilds: [...client.guilds.cache.values()],
      missingGuildIds: [],
    };
  }

  const guilds = [];
  const missingGuildIds = [];

  for (const guildId of guildIds) {
    const normalizedGuildId = String(guildId || '').trim();
    if (!normalizedGuildId) continue;

    const cached = client.guilds.cache.get(normalizedGuildId);
    if (cached) {
      guilds.push(cached);
      continue;
    }

    const fetched = await client.guilds.fetch(normalizedGuildId).catch(() => null);
    if (fetched) {
      guilds.push(fetched);
      continue;
    }

    missingGuildIds.push(normalizedGuildId);
  }

  return { guilds, missingGuildIds };
}

async function syncGuildCommands(client, options = {}) {
  const { guildIds = [] } = options;
  const payload = getApplicationCommandPayload();
  const resolved = await resolveTargetGuilds(client, guildIds);
  const registrations = await Promise.allSettled(
    resolved.guilds.map((guild) => guild.commands.set(payload)),
  );

  const details = registrations.map((result, index) => ({
    guildId: resolved.guilds[index]?.id || 'unknown',
    guildName: resolved.guilds[index]?.name || 'unknown',
    status: result.status,
    error: result.status === 'rejected' ? result.reason?.message || 'unknown error' : null,
  }));

  for (const guildId of resolved.missingGuildIds) {
    details.push({
      guildId,
      guildName: 'unknown',
      status: 'rejected',
      error: 'Bot nie widzi tego serwera albo nie jest na nim obecny.',
    });
  }

  const ok = details.filter((entry) => entry.status === 'fulfilled').length;
  const fail = details.length - ok;

  return {
    scope: 'guild',
    payloadCount: payload.length,
    totalGuilds: details.length,
    ok,
    fail,
    details,
  };
}

async function syncGlobalCommands(client, options = {}) {
  const payload = options.clear === true ? [] : getApplicationCommandPayload();
  const result = await client.application.commands.set(payload);
  const commandCount = Array.isArray(result)
    ? result.length
    : (typeof result?.size === 'number' ? result.size : payload.length);

  return {
    scope: 'global',
    payloadCount: payload.length,
    totalGuilds: 0,
    ok: 1,
    fail: 0,
    commandCount,
    cleared: options.clear === true,
  };
}

function formatCommandSyncReport(report) {
  if (!report) {
    return ['Brak raportu sync komend.'];
  }

  if (report.scope === 'global') {
    if (report.cleared) {
      return ['Globalne komendy zostaly wyczyszczone.'];
    }
    return [`Globalne komendy zsynchronizowane: ${report.commandCount}.`];
  }

  const lines = [
    `Guild sync: komend=${report.payloadCount}, serwerow=${report.totalGuilds}, sukces=${report.ok}, bledy=${report.fail}`,
  ];

  for (const detail of report.details || []) {
    if (detail.status === 'fulfilled') {
      continue;
    }
    lines.push(`- ${detail.guildName} (${detail.guildId}): ${detail.error}`);
  }

  return lines;
}

module.exports = {
  formatCommandSyncReport,
  syncGlobalCommands,
  syncGuildCommands,
};
