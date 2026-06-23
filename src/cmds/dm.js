const { ApplicationCommandOptionType, PermissionFlagsBits } = require('discord.js');

const dmCommand = {
  name: 'dm',
  description: 'Wysyla prywatna wiadomosc do usera albo roli',
  dm_permission: false,
  default_member_permissions: PermissionFlagsBits.Administrator.toString(),
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'message',
      description: 'Tresc wiadomosci prywatnej',
      required: true,
    },
    {
      type: ApplicationCommandOptionType.User,
      name: 'user',
      description: 'Jeden odbiorca wiadomosci',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Role,
      name: 'role',
      description: 'Rola, do ktorej czlonkow wyslac wiadomosc',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Boolean,
      name: 'confirm',
      description: 'Wymagane przy wysylce masowej do roli',
      required: false,
    },
  ],
};

const BULK_DM_CONFIRM_THRESHOLD = 10;

function replyEphemeral(interaction, content) {
  return interaction.reply({
    content,
    ephemeral: true,
  });
}

function normalizeMessage(value) {
  const text = String(value || '').trim();
  if (!text) {
    throw new Error('Wiadomosc nie moze byc pusta.');
  }
  if (text.length > 2000) {
    throw new Error('Wiadomosc jest za dluga. Limit DM to 2000 znakow.');
  }
  return text;
}

function formatFailurePreview(failures, limit = 5) {
  if (!Array.isArray(failures) || failures.length === 0) {
    return '';
  }

  const preview = failures
    .slice(0, limit)
    .map((entry) => `${entry.label}: ${entry.reason}`)
    .join('\n');

  if (failures.length <= limit) {
    return preview;
  }

  return `${preview}\n... i jeszcze ${failures.length - limit} bledow`;
}

async function executeDmCommand(interaction) {
  if (!interaction.inGuild()) {
    await replyEphemeral(interaction, 'Ta komenda dziala tylko na serwerze.');
    return;
  }

  if (!interaction.memberPermissions?.has(PermissionFlagsBits.Administrator)) {
    await replyEphemeral(interaction, 'Brak uprawnien Administratora.');
    return;
  }

  const targetUser = interaction.options.getUser('user');
  const targetRole = interaction.options.getRole('role');
  const confirmBulkSend = interaction.options?.getBoolean?.('confirm', false) === true;

  if ((targetUser && targetRole) || (!targetUser && !targetRole)) {
    await replyEphemeral(interaction, 'Podaj dokladnie jedno pole targetu: `user` albo `role`.');
    return;
  }

  let message;
  try {
    message = normalizeMessage(interaction.options.getString('message', true));
  } catch (error) {
    await replyEphemeral(interaction, error.message);
    return;
  }

  await interaction.deferReply({ ephemeral: true });

  if (targetUser) {
    try {
      await targetUser.send({ content: message });
      await interaction.editReply(`Wyslano DM do <@${targetUser.id}>.`);
    } catch (error) {
      await interaction.editReply(`Nie udalo sie wyslac DM do <@${targetUser.id}>: ${error.message}`);
    }
    return;
  }

  const guild = interaction.guild;
  const members = await guild.members.fetch().catch(() => null);
  if (!members) {
    await interaction.editReply('Nie udalo sie pobrac listy czlonkow serwera.');
    return;
  }

  const memberList = typeof members.values === 'function' ? [...members.values()] : [];
  const targets = memberList
    .filter((member) => member?.roles?.cache?.has(targetRole.id) && !member.user?.bot)
    .map((member) => member.user);

  if (targets.length === 0) {
    await interaction.editReply(`Rola <@&${targetRole.id}> nie ma zadnych odbiorcow do wysylki.`);
    return;
  }

  if (targets.length > BULK_DM_CONFIRM_THRESHOLD && !confirmBulkSend) {
    const preview = targets
      .slice(0, 10)
      .map((user) => `- @${user.username}`)
      .join('\n');

    await interaction.editReply([
      `Ta akcja wysle DM do ${targets.length} osob z roli <@&${targetRole.id}>.`,
      'Dla bezpieczenstwa masowa wysylka wymaga dodatkowego potwierdzenia.',
      '',
      'Podglad odbiorcow:',
      preview,
      '',
      'Uruchom komende ponownie z `confirm:true`, jesli chcesz kontynuowac.',
    ].join('\n'));
    return;
  }

  let sent = 0;
  const failures = [];

  for (const user of targets) {
    try {
      await user.send({ content: message });
      sent += 1;
    } catch (error) {
      failures.push({
        label: `@${user.username}`,
        reason: error.message,
      });
    }
  }

  const lines = [
    `Wysylka DM do roli <@&${targetRole.id}> zakonczona.`,
    `Targety: ${targets.length}`,
    `Sukces: ${sent}`,
    `Bledy: ${failures.length}`,
  ];

  const failurePreview = formatFailurePreview(failures);
  if (failurePreview) {
    lines.push('');
    lines.push('Podglad bledow:');
    lines.push(failurePreview);
  }

  await interaction.editReply(lines.join('\n'));
}

module.exports = {
  dmCommand,
  executeDmCommand,
};
