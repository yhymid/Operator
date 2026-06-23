const { ApplicationCommandOptionType } = require('discord.js');
const { isOwner } = require('../core/owners');

const cleanCommand = {
  name: 'clean',
  description: 'Komendy czyszczenia',
  dm_permission: false,
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Tryb komendy clean',
      required: true,
      choices: [
        { name: 'now', value: 'now' },
        { name: 'set', value: 'set' },
        { name: 'reset', value: 'reset' },
        { name: 'status', value: 'status' },
      ],
    },
  ],
};

function formatStoredDate(value, timezone = 'Europe/Warsaw') {
  if (!value) return 'brak';

  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }

  return parsed.toLocaleString('pl-PL', {
    timeZone: timezone,
  });
}

function formatPermanentChannels(channels) {
  if (!Array.isArray(channels) || channels.length === 0) {
    return 'brak';
  }

  return channels.map((entry) => {
    const detail = [];
    if (entry.deleteLimit > 0) {
      detail.push(`limit=${entry.deleteLimit}`);
    }
    if (entry.deletePinned) {
      detail.push('pinned=yes');
    }
    return `<#${entry.channelId}>${detail.length > 0 ? ` (${detail.join(', ')})` : ''}`;
  }).join(', ');
}

function formatQueuedChannels(channels, timezone) {
  if (!Array.isArray(channels) || channels.length === 0) {
    return 'brak';
  }

  return channels.map((entry) => {
    const parts = [`<#${entry.channelId}>`];
    if (entry.queuedAt) {
      parts.push(`queued=${formatStoredDate(entry.queuedAt, timezone)}`);
    }
    if (entry.queuedBy) {
      parts.push(`by=<@${entry.queuedBy}>`);
    }
    return parts.join(' ');
  }).join('\n');
}

async function executeCleanCommand(interaction, { cleaner }) {
  if (!interaction.inGuild()) {
    await interaction.reply({
      content: 'Ta komenda dziala tylko na serwerze.',
      ephemeral: true,
    });
    return;
  }

  if (!isOwner(interaction.user.id)) {
    await interaction.reply({
      content: 'Ta komenda jest tylko dla ownera.',
      ephemeral: true,
    });
    return;
  }

  const mode = String(interaction.options.getString('mode', true) || '').toLowerCase();

  if (mode === 'status') {
    const status = cleaner.getStatusSnapshot(interaction.guildId);
    const lines = [
      `AutoClean: ${status.enabled ? 'wlaczony' : 'wylaczony'}`,
      `CRON: ${status.schedule}`,
      `Timezone: ${status.timezone}`,
      `Najblizszy cleanup: ${status.nextRunText}`,
      `Next run cache: ${formatStoredDate(status.nextRunAt, status.timezone)}`,
      `Dry run: ${status.dryRun ? 'tak' : 'nie'}`,
      `Interwal: ${status.deleteIntervalText}`,
      `Minimalny wiek wiadomosci: ${status.deleteMessagesOlderThanDays || 0} dni`,
      `Delete limit: ${status.deleteLimit || 'brak'}`,
      `Delete pinned: ${status.deletePinned ? 'tak' : 'nie'}`,
      `Whitelist users: ${status.whitelistUsers}`,
      `Whitelist roles: ${status.whitelistRoles}`,
      `Notify before delete: ${status.notifyBeforeDelete}`,
      `Log channel: ${status.logChannelId ? `<#${status.logChannelId}>` : 'brak'}`,
      `Stale kanaly z configu (${status.permanentChannels.length}): ${formatPermanentChannels(status.permanentChannels)}`,
      `Tymczasowo zakolejkowane (${status.queuedChannelsCount}):`,
      formatQueuedChannels(status.queuedChannels, status.timezone),
      `Ostatni cleanup: ${formatStoredDate(status.lastRunAt, status.timezone)}`,
      `Ostatni trigger: ${status.lastTrigger || 'brak'}`,
      `Ostatnio usuniete: ${status.lastDeleted ?? 0}`,
      `Ostatni czas wykonania: ${status.lastDurationMs ?? 'brak'} ms`,
      `Ostatni blad: ${status.lastError || 'brak'}`,
      `Uruchomienia w tej sesji: ${status.runs}`,
      `Usuniete lacznie w tej sesji: ${status.totalDeleted}`,
    ];
    await interaction.reply({ content: lines.join('\n'), ephemeral: true });
    return;
  }

  const channel = interaction.channel;
  if (!channel || !channel.isTextBased()) {
    await interaction.reply({
      content: 'Ten kanal nie obsluguje wiadomosci tekstowych.',
      ephemeral: true,
    });
    return;
  }

  if (mode === 'set') {
    const queued = cleaner.queueChannelForNextCleanup(channel, interaction.user.id);
    const status = cleaner.getStatusSnapshot(interaction.guildId);
    const nextRun = formatStoredDate(status.nextRunAt, status.timezone);

    await interaction.reply({
      content: queued.alreadyQueued
        ? `Kanal <#${channel.id}> juz czeka na jednorazowy cleanup. Najblizszy termin: ${nextRun}.`
        : `Dodano <#${channel.id}> do jednorazowego cleanupu przy najblizszym terminie. Najblizszy termin: ${nextRun}.`,
      ephemeral: true,
    });
    return;
  }

  if (mode === 'reset') {
    const removed = cleaner.resetQueuedChannel(channel);

    await interaction.reply({
      content: removed.removed
        ? `Usunieto <#${channel.id}> z kolejki jednorazowego cleanupu.`
        : `Kanal <#${channel.id}> nie byl zakolejkowany do jednorazowego cleanupu.`,
      ephemeral: true,
    });
    return;
  }

  if (mode !== 'now') {
    await interaction.reply({
      content: `Nieznany mode clean: ${mode}.`,
      ephemeral: true,
    });
    return;
  }

  await interaction.deferReply({ ephemeral: true });

  const result = await cleaner.runCleanupForChannel(channel, 'slash');

  if (result === null) {
    await interaction.editReply('Czyszczenie juz trwa. Sprobuj ponownie za chwile.');
    return;
  }

  if (result.error) {
    await interaction.editReply(`Blad czyszczenia: ${result.error}`);
    return;
  }

  await interaction.editReply(`Wyczyszczono kanal <#${channel.id}>. Usunieto ${result.deleted} wiadomosci.`);
}

module.exports = {
  cleanCommand,
  executeCleanCommand,
};
