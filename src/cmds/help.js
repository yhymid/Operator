const { isOwner } = require('../core/owners');

const HELP_BUTTON_PREFIX = 'help_panel';
const COMPONENTS_V2_FLAG = 32768;
const EPHEMERAL_FLAG = 64;
const BUTTON_STYLE = {
  secondary: 2,
  success: 3,
};

function makeHelpButtonId(page, userId) {
  return `${HELP_BUTTON_PREFIX}:${page}:${userId}`;
}

function parseHelpButtonId(customId) {
  const [prefix, page, ownerUserId] = String(customId || '').split(':');
  if (prefix !== HELP_BUTTON_PREFIX || !page || !ownerUserId) return null;
  return { page, ownerUserId };
}

function getHelpSectionContent(page, canUseOwnerSection) {
  const section = String(page || 'overview').toLowerCase();

  if (section === 'music') {
    return [
      '### Muzyka',
      '`/music mode:play query:<fraza lub link>` albo `audio:<plik z Discorda>`',
      '`/music mode:queue|nowplaying|skip|stop`',
      '`/music mode:pause|resume`',
      '`/music mode:volume value:<+10|-10|80>`',
      '`/music mode:remove position:<numer>`',
      '`/music mode:clear|reconnect`',
    ].join('\n');
  }

  if (section === 'moderation') {
    return [
      '### Moderacja',
      '`/automod status|test`',
      '`/automod rule list|add|remove`',
      '`/verify mode:setup|status|resend_panel|exception_add|exception_remove|expire_user|cleanup_state`',
      '`/logs mode:setup|status|enable|disable|channel|retention|include_bots|preview|export_day|categories|purge|test`',
      '`/dm user:<user>|role:<role> message:<text> confirm:true`',
    ].join('\n');
  }

  if (section === 'settings') {
    return [
      '### Ustawienia',
      '`/settings mode:show`',
      '`/settings mode:show section:<autoclean|automod|verify>`',
      '`/settings mode:set section:<autoclean|automod|verify> option:<opcja> value:<wartosc>`',
      '`/health`',
      '`/debug mode:<music|permissions|logs>`',
    ].join('\n');
  }

  if (section === 'owner') {
    if (!canUseOwnerSection) {
      return [
        '### Owner',
        'Ta sekcja jest dostepna tylko dla ownera bota.',
      ].join('\n');
    }

    return [
      '### Owner',
      '`/clean mode:now|set|reset|status`',
      '`/cache mode:serverinfo id:<guild_id>`',
      '`/announce mode:create|now|list|edit|delete`',
      '`/announce mode:profile_create|profile_list|profile_edit|profile_delete`',
    ].join('\n');
  }

  return [
    '### Start',
    '`/help section:<sekcja>` - otwiera od razu wybrany obszar',
    '`/music mode:play ...` - odtwarzanie, kolejka i recovery',
    '`/automod ...`, `/verify ...`, `/logs ...`, `/dm ...` - moderacja, verify i full logs',
    '`/settings ...`, `/health`, `/debug ...` - konfiguracja i diagnostyka',
    '`/announce ...` - planowane i natychmiastowe announcementy',
    '`/clean ...`, `/cache ...` - narzedzia ownera',
  ].join('\n');
}

function buildHelpPanelPayload(userId, page = 'overview') {
  const selectedPage = String(page || 'overview').toLowerCase();
  const canUseOwnerSection = isOwner(userId);
  const pages = [
    { id: 'overview', label: 'Start' },
    { id: 'music', label: 'Muzyka' },
    { id: 'moderation', label: 'Moderacja' },
    { id: 'settings', label: 'Ustawienia' },
    { id: 'owner', label: 'Owner', ownerOnly: true },
  ];

  return {
    flags: COMPONENTS_V2_FLAG | EPHEMERAL_FLAG,
    components: [
      {
        type: 17,
        components: [
          {
            type: 10,
            content: [
              '## Operator Help',
              'Interaktywny panel komend. Wybierz sekcje ponizej albo uruchom `/help section:<sekcja>`.',
              '',
              getHelpSectionContent(selectedPage, canUseOwnerSection),
            ].join('\n'),
          },
          {
            type: 1,
            components: pages.map((entry) => {
              const active = selectedPage === entry.id;
              const locked = entry.ownerOnly && !canUseOwnerSection;
              return {
                type: 2,
                custom_id: makeHelpButtonId(entry.id, userId),
                label: entry.label,
                style: active ? BUTTON_STYLE.success : BUTTON_STYLE.secondary,
                disabled: locked,
              };
            }),
          },
        ],
      },
    ],
  };
}

async function executeHelpComponentInteraction(interaction) {
  if (!interaction.isButton()) return false;

  const parsed = parseHelpButtonId(interaction.customId);
  if (!parsed) return false;

  if (interaction.user.id !== parsed.ownerUserId) {
    await interaction.reply({
      content: 'Ten panel pomocy nalezy do innego uzytkownika. Uzyj /help.',
      ephemeral: true,
    });
    return true;
  }

  await interaction.update(buildHelpPanelPayload(interaction.user.id, parsed.page));
  return true;
}

module.exports = {
  buildHelpPanelPayload,
  executeHelpComponentInteraction,
};
