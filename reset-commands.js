const { REST, Routes } = require("discord.js");
require("dotenv").config();

const rest = new REST({ version: "10" }).setToken(process.env.TOKEN);

(async () => {
    try {
        console.log("⏳ Resetuję komendy...");

        // 🔹 Guild reset
        await rest.put(
            Routes.applicationGuildCommands(process.env.CLIENT_ID, process.env.GUILD_ID),
            { body: [] },
        );
        console.log("🗑️ Wszystkie komendy na serwerze zostały usunięte!");

        // 🔹 Global reset (opcjonalnie)
        await rest.put(
            Routes.applicationCommands(process.env.CLIENT_ID),
            { body: [] },
        );
        console.log("🗑️ Wszystkie globalne komendy zostały usunięte!");

    } catch (error) {
        console.error(error);
    }
})();