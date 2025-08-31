const fs = require("fs");
const path = require("path");
const { Client, GatewayIntentBits, Collection, REST, Routes } = require("discord.js");
require("dotenv").config();

const client = new Client({ intents: [GatewayIntentBits.Guilds] });
client.commands = new Collection();

const commands = [];
const commandsPath = path.join(__dirname, "src", "commands");
const commandFiles = fs.readdirSync(commandsPath).filter(file => file.endsWith(".js"));

// Wczytanie komend z folderu
for (const file of commandFiles) {
    const filePath = path.join(commandsPath, file);
    const command = require(filePath);
    if ("data" in command && "execute" in command) {
        client.commands.set(command.data.name, command);
        commands.push(command.data.toJSON());
    } else {
        console.log(`[WARN] Komenda w ${filePath} nie ma "data" lub "execute"`);
    }
}

// Rejestracja komend w API Discord
const rest = new REST({ version: "10" }).setToken(process.env.TOKEN);

(async () => {
    try {
        console.log("⏳ Rejestracja (deploy) komend slash...");

        await rest.put(
            Routes.applicationGuildCommands(process.env.CLIENT_ID, process.env.GUILD_ID),
            { body: commands },
        );
        console.log("✅ Slash commands zarejestrowane na serwerze testowym!");

        //await rest.put(
        //    Routes.applicationCommands(process.env.CLIENT_ID),
        //    { body: commands },
        //);
        //console.log("✅ Slash commands zarejestrowane!");
    } catch (error) {
        console.error(error);
    }
})();

// Obsługa eventów
client.once("ready", () => {
    console.log(`✅ Zalogowano jako ${client.user.tag}`);
});

client.on("interactionCreate", async interaction => {
    if (!interaction.isChatInputCommand()) return;

    const command = client.commands.get(interaction.commandName);
    if (!command) return;

    try {
        await command.execute(interaction);
    } catch (error) {
        console.error(error);
        await interaction.reply({ content: "❌ Wystąpił błąd przy wykonywaniu komendy!", ephemeral: true });
    }
});

client.login(process.env.TOKEN);