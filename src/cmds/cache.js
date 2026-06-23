const fs = require('fs');
const path = require('path');
const {
  ApplicationCommandOptionType,
  AttachmentBuilder,
} = require('discord.js');
const { isOwner } = require('../core/owners');

const cacheCommand = {
  name: 'cache',
  description: 'Narzedzia cache',
  dm_permission: false,
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Tryb komendy cache',
      required: true,
      choices: [
        { name: 'serverinfo', value: 'serverinfo' },
      ],
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'id',
      description: 'Guild ID (np. 1008460463148900373)',
      required: false,
    },
  ],
};

function isSnowflakeLike(value) {
  return /^\d{16,22}$/.test(value);
}

async function executeCacheCommand(interaction, { cacheDir, logger }) {
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
  if (mode !== 'serverinfo') {
    await interaction.reply({
      content: `Nieznany mode cache: ${mode}.`,
      ephemeral: true,
    });
    return;
  }

  const id = String(interaction.options.getString('id', false) || '').trim();
  if (!id) {
    await interaction.reply({
      content: 'Dla mode=serverinfo podaj `id`.',
      ephemeral: true,
    });
    return;
  }
  if (!isSnowflakeLike(id)) {
    await interaction.reply({
      content: 'Niepoprawne ID. Podaj poprawny Discord ID (cyfry).',
      ephemeral: true,
    });
    return;
  }

  const filePath = path.join(cacheDir, `${id}.json`);
  if (!fs.existsSync(filePath)) {
    await interaction.reply({
      content: `Brak cache dla ID \`${id}\` (${filePath}).`,
      ephemeral: true,
    });
    return;
  }

  try {
    const parsed = JSON.parse(fs.readFileSync(filePath, 'utf8'));
    const pretty = JSON.stringify(parsed, null, 2);
    const wrapped = `\`\`\`json\n${pretty}\n\`\`\``;

    if (wrapped.length <= 1900) {
      await interaction.reply({
        content: wrapped,
        ephemeral: true,
      });
      return;
    }

    const attachment = new AttachmentBuilder(Buffer.from(pretty, 'utf8'), {
      name: `server-${id}.json`,
    });

    await interaction.reply({
      content: `Cache jest za dlugi na wiadomosc. Wysylam plik \`server-${id}.json\`.`,
      files: [attachment],
      ephemeral: true,
    });
  } catch (error) {
    logger?.error?.(`Cache serverinfo error (${id}): ${error.message}`);
    await interaction.reply({
      content: `Nie udalo sie odczytac cache dla ID \`${id}\`.`,
      ephemeral: true,
    });
  }
}

module.exports = {
  cacheCommand,
  executeCacheCommand,
};
