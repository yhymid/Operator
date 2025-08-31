const { SlashCommandBuilder, EmbedBuilder } = require("discord.js");

module.exports = {
    data: new SlashCommandBuilder()
        .setName("pirates")
        .setDescription("Wyświetla Strony Pirackie"),

    async execute(interaction) {
        // 🔹 Tutaj ustawiasz sobie jak embed ma wyglądać
        const embed = new EmbedBuilder()
            .setTitle("Strony Pirackie")
            .setDescription("Tutaj znajdziesz wszystkie przydatne linki do stron pirackich.")
            .addFields(
                { name: "Encyklopedia FMHY", value: "[Kliknij tutaj](https://fmhy.net/)", inline: true },
                { name: "FitGirl Repack", value: "[Kliknij tutaj](https://fitgirl-repacks.site/popular-repacks/)", inline: true },
                { name: "Dodi Repack", value: "[Kliknij tutaj](https://dodi-repacks.site/)", inline: true },
            )
            .setColor("#ff0000")
            .setFooter({ text: `Embed wykonany przez ${interaction.user.tag}` })
            .setTimestamp();

        await interaction.reply({ embeds: [embed] });
    }
};