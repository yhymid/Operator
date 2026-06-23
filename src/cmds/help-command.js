const { SlashCommandBuilder } = require('discord.js');

const HELP_SECTION_CHOICES = [
  ['Start', 'overview'],
  ['Muzyka', 'music'],
  ['Moderacja', 'moderation'],
  ['Ustawienia', 'settings'],
  ['Owner', 'owner'],
];

const helpCommand = new SlashCommandBuilder()
  .setName('help')
  .setDescription('Pokazuje interaktywny panel pomocy')
  .addStringOption((option) => option
    .setName('section')
    .setDescription('Od razu otworz wybrana sekcje')
    .setRequired(false)
    .addChoices(...HELP_SECTION_CHOICES.map(([name, value]) => ({ name, value }))))
  .setDMPermission(false)
  .toJSON();

async function executeHelpCommand(interaction, { buildHelpPayload }) {
  const selectedSection = interaction.options?.getString?.('section', false) || 'overview';
  await interaction.reply(buildHelpPayload(interaction.user.id, selectedSection));
}

module.exports = {
  helpCommand,
  executeHelpCommand,
};
