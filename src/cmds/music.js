const {
  ApplicationCommandOptionType,
  PermissionFlagsBits,
} = require('discord.js');

const MUSIC_BUTTON_PREFIX = 'music_panel';
const MUSIC_SELECT_PREFIX = 'music_select';
const COMPONENTS_V2_FLAG = 32768;
const BUTTON_STYLE = {
  primary: 1,
  secondary: 2,
  success: 3,
  danger: 4,
};
const AUDIO_FILE_RE = /\.(aac|aiff?|alac|flac|m4a|mp3|oga|ogg|opus|wav|weba|wma)(\?|$)/i;

const musicCommand = {
  name: 'music',
  description: 'Odtwarzanie muzyki',
  dm_permission: false,
  options: [
    {
      type: ApplicationCommandOptionType.String,
      name: 'mode',
      description: 'Tryb komendy music',
      required: true,
      choices: [
        { name: 'play', value: 'play' },
        { name: 'pause', value: 'pause' },
        { name: 'resume', value: 'resume' },
        { name: 'volume', value: 'volume' },
        { name: 'remove', value: 'remove' },
        { name: 'clear', value: 'clear' },
        { name: 'reconnect', value: 'reconnect' },
        { name: 'skip', value: 'skip' },
        { name: 'stop', value: 'stop' },
        { name: 'queue', value: 'queue' },
        { name: 'nowplaying', value: 'nowplaying' },
      ],
    },
    {
      type: ApplicationCommandOptionType.String,
      name: 'query',
      description: 'Link lub fraza (YouTube/SoundCloud/Spotify/radio)',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Attachment,
      name: 'audio',
      description: 'Plik audio wrzucony na Discorda (dla mode=play)',
      required: false,
    },
    {
      type: ApplicationCommandOptionType.Integer,
      name: 'value',
      description: 'Zmiana glosnosci dla mode=volume, np. 10 albo -10',
      required: false,
      min_value: -100,
      max_value: 100,
    },
    {
      type: ApplicationCommandOptionType.Integer,
      name: 'position',
      description: 'Pozycja w kolejce dla mode=remove (1 = pierwszy po aktualnym)',
      required: false,
      min_value: 1,
      max_value: 500,
    },
  ],
};

function formatDuration(seconds) {
  const normalized = Number(seconds) || 0;
  if (normalized <= 0) return '?:??';

  const hours = Math.floor(normalized / 3600);
  const minutes = Math.floor((normalized % 3600) / 60);
  const secs = normalized % 60;

  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
  }

  return `${minutes}:${String(secs).padStart(2, '0')}`;
}

function formatTrackLine(track, index = null) {
  const prefix = index === null ? '' : `${index}. `;
  const duration = formatDuration(track.durationSec);
  return `${prefix}${track.title} (${duration})`;
}

function formatProgress(snapshot) {
  if (!snapshot?.currentTrack) return '?:??';
  const position = formatDuration(snapshot.positionSec || 0);
  const duration = formatDuration(snapshot.currentTrack.durationSec || 0);
  return `${position} / ${duration}`;
}

function formatRepeatMode(value) {
  if (value === 'track') return 'utwor';
  if (value === 'queue') return 'kolejka';
  return 'off';
}

function truncateText(value, max = 80) {
  const raw = String(value || '').trim();
  if (raw.length <= max) return raw;
  return `${raw.slice(0, Math.max(0, max - 3)).trimEnd()}...`;
}

function buildProgressBar(snapshot, slots = 12) {
  const duration = Number(snapshot?.currentTrack?.durationSec || 0);
  const position = Number(snapshot?.positionSec || 0);
  if (duration <= 0) return `${'▱'.repeat(slots)} 0%`;

  const ratio = Math.max(0, Math.min(1, position / duration));
  const filled = Math.min(slots, Math.round(ratio * slots));
  const percent = Math.round(ratio * 100);
  return `${'▰'.repeat(filled)}${'▱'.repeat(Math.max(0, slots - filled))} ${percent}%`;
}

function getActiveFilterLabels(snapshot) {
  const labels = [];
  const eqPreset = String(snapshot?.filters?.eqPreset || 'flat');
  if (eqPreset !== 'flat') {
    labels.push(eqPreset[0].toUpperCase() + eqPreset.slice(1));
  }
  if (snapshot?.filters?.vaporwave === true) labels.push('Vaporwave');
  if (snapshot?.filters?.eightD === true) labels.push('8D');
  if (snapshot?.filters?.karaoke === true) labels.push('Karaoke');
  if (snapshot?.filters?.softLimiter === true) labels.push('SoftLimiter');
  return labels;
}

function makeMusicButtonId(action, guildId) {
  return `${MUSIC_BUTTON_PREFIX}:${action}:${guildId}`;
}

function makeMusicSelectId(action, guildId) {
  return `${MUSIC_SELECT_PREFIX}:${action}:${guildId}`;
}

function parseMusicComponentId(customId) {
  const [prefix, action, guildId] = String(customId || '').split(':');
  if (!action || !guildId) return null;

  if (prefix === MUSIC_BUTTON_PREFIX) {
    return { kind: 'button', action, guildId };
  }
  if (prefix === MUSIC_SELECT_PREFIX) {
    return { kind: 'select', action, guildId };
  }
  return null;
}

function buildEqOptions(selectedPreset) {
  const active = String(selectedPreset || 'flat');
  return [
    {
      label: 'Flat',
      value: 'flat',
      description: 'Neutralny dzwiek',
      default: active === 'flat',
    },
    {
      label: 'Bassboost',
      value: 'bassboost',
      description: 'Mocniejszy dol',
      default: active === 'bassboost',
    },
    {
      label: 'Vocal',
      value: 'vocal',
      description: 'Lepsza czytelnosc wokalu',
      default: active === 'vocal',
    },
    {
      label: 'Treble',
      value: 'treble',
      description: 'Jasniejsze wysokie tony',
      default: active === 'treble',
    },
    {
      label: 'Nightcore',
      value: 'nightcore',
      description: 'Szybciej + wyzszy pitch',
      default: active === 'nightcore',
    },
  ];
}

function buildFxOptions(snapshot) {
  return [
    {
      label: 'Vaporwave',
      value: 'vaporwave',
      description: 'Wolniej i cieplej',
      default: false,
    },
    {
      label: '8D',
      value: '8d',
      description: 'Efekt obrotu stereo',
      default: false,
    },
    {
      label: 'Karaoke',
      value: 'karaoke',
      description: 'Probuj stlumic wokal',
      default: false,
    },
    {
      label: 'SoftLimiter',
      value: 'softlimiter',
      description: 'Lagodny low-pass',
      default: false,
    },
    {
      label: 'FX Off',
      value: 'off',
      description: 'Wylacz wszystkie FX',
      default: false,
    },
  ];
}

function buildLyricsContent(snapshot) {
  if (!snapshot?.currentTrack?.title) return 'Brak utworu do wyszukania tekstu.';
  const title = String(snapshot.currentTrack.title);
  const url = `https://genius.com/search?q=${encodeURIComponent(title)}`;
  return `Tekst (wyszukiwanie): ${url}`;
}

function buildMusicPanelPayload(snapshot) {
  const current = snapshot?.currentTrack || null;
  const title = truncateText(current?.title || 'Nic nie gra', 80);
  const status = snapshot?.paused ? 'Pauza' : (snapshot?.playing ? 'Gra' : 'Stop');
  const nextLines = snapshot?.queue?.length
    ? snapshot.queue.slice(0, 5).map((track, index) => `${index + 1}. ${truncateText(track.title, 70)}`).join('\n')
    : 'Brak nastepnych utworow.';
  const playPauseLabel = snapshot?.paused ? 'Play' : 'Pause';
  const muteLabel = snapshot?.muted ? 'Unmute' : 'Mute';
  const disabled = !snapshot?.currentTrack;
  const guildId = snapshot?.guildId || '0';
  const activeFilters = getActiveFilterLabels(snapshot);
  const eqPreset = String(snapshot?.filters?.eqPreset || 'flat');
  const activeFx = [
    snapshot?.filters?.vaporwave === true ? 'Vaporwave' : null,
    snapshot?.filters?.eightD === true ? '8D' : null,
    snapshot?.filters?.karaoke === true ? 'Karaoke' : null,
    snapshot?.filters?.softLimiter === true ? 'SoftLimiter' : null,
  ].filter(Boolean);
  const hasActiveFx = Boolean(snapshot?.filters?.vaporwave || snapshot?.filters?.eightD || snapshot?.filters?.karaoke || snapshot?.filters?.softLimiter);
  const queueCount = Number(snapshot?.queue?.length || 0);

  return {
    flags: COMPONENTS_V2_FLAG,
    components: [
      {
        type: 17,
        components: [
          {
            type: 10,
            content: [
              '## Now Playing',
              `**${title}**`,
              current?.url ? current.url : null,
              '',
              `Czas: **${formatProgress(snapshot)}**`,
              buildProgressBar(snapshot),
              `Loop: ${formatRepeatMode(snapshot?.repeatMode)} | Vol: ${snapshot?.volume ?? '?'}% | Filters: ${activeFilters.join(', ') || 'off'} | Autoplay: ${snapshot?.autoplay ? 'ON' : 'OFF'}`,
              `Status: **${status}**`,
              `Kanal: ${snapshot?.voiceChannelId ? `<#${snapshot.voiceChannelId}>` : 'brak'}`,
              '',
              '**Kolejka**',
              nextLines,
            ].filter((line) => line !== null).join('\n'),
          },
          {
            type: 1,
            components: [
              {
                type: 2,
                custom_id: makeMusicButtonId('loop', guildId),
                label: 'Loop',
                style: BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('queue', guildId),
                label: `Queue (${queueCount})`,
                style: BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('playpause', guildId),
                label: playPauseLabel,
                style: snapshot?.paused ? BUTTON_STYLE.success : BUTTON_STYLE.primary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('stop', guildId),
                label: 'Stop',
                style: BUTTON_STYLE.danger,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('shuffle', guildId),
                label: 'Shuffle',
                style: BUTTON_STYLE.secondary,
                disabled: disabled || !snapshot?.queue || snapshot.queue.length < 2,
              },
            ],
          },
          {
            type: 1,
            components: [
              {
                type: 2,
                custom_id: makeMusicButtonId('eq', guildId),
                label: 'EQ',
                style: eqPreset !== 'flat' ? BUTTON_STYLE.success : BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('volumedown', guildId),
                label: 'Volume -',
                style: BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('mute', guildId),
                label: muteLabel,
                style: snapshot?.muted ? BUTTON_STYLE.success : BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('volumeup', guildId),
                label: 'Volume +',
                style: BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('fx', guildId),
                label: 'FX',
                style: hasActiveFx ? BUTTON_STYLE.success : BUTTON_STYLE.secondary,
                disabled,
              },
            ],
          },
          {
            type: 1,
            components: [
              {
                type: 2,
                custom_id: makeMusicButtonId('autoplay', guildId),
                label: 'Autoplay',
                style: snapshot?.autoplay ? BUTTON_STYLE.success : BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('lyrics', guildId),
                label: 'Lyrics',
                style: BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('savequeue', guildId),
                label: 'Save Queue',
                style: BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('restorequeue', guildId),
                label: 'Restore Queue',
                style: BUTTON_STYLE.secondary,
                disabled,
              },
              {
                type: 2,
                custom_id: makeMusicButtonId('refresh', guildId),
                label: 'Refresh',
                style: BUTTON_STYLE.secondary,
                disabled,
              },
            ],
          },
          {
            type: 1,
            components: [
              {
                type: 3,
                custom_id: makeMusicSelectId('eq', guildId),
                placeholder: `EQ preset: ${eqPreset}`,
                options: buildEqOptions(eqPreset),
                disabled,
              },
            ],
          },
          {
            type: 1,
            components: [
              {
                type: 3,
                custom_id: makeMusicSelectId('fx', guildId),
                placeholder: activeFx.length ? `FX: ${activeFx.join(', ')}` : 'FX: wybierz efekt',
                options: buildFxOptions(snapshot),
                disabled,
              },
            ],
          },
        ],
      },
    ],
  };
}

function buildQueuePreviewContent(snapshot) {
  if (!snapshot?.queue?.length) return 'Kolejka jest pusta.';
  const lines = snapshot.queue.slice(0, 10).map((track, index) => formatTrackLine(track, index + 1));
  if (snapshot.queue.length > lines.length) {
    lines.push(`... i jeszcze ${snapshot.queue.length - lines.length}`);
  }
  return lines.join('\n');
}

function getMemberVoiceChannel(interaction) {
  return interaction.member?.voice?.channel || null;
}

function userIsInSameVoiceChannel(interaction, voiceChannelId) {
  const channel = getMemberVoiceChannel(interaction);
  return Boolean(channel && channel.id === voiceChannelId);
}

function requireSameVoiceChannel(interaction, voiceChannelId) {
  if (voiceChannelId && !userIsInSameVoiceChannel(interaction, voiceChannelId)) {
    return 'Musisz byc na tym samym kanale glosowym co bot.';
  }

  if (!voiceChannelId && !getMemberVoiceChannel(interaction)) {
    return 'Dolacz do kanalu glosowego, zeby sterowac ta sesja.';
  }

  return null;
}

function isLikelyAudioAttachment(attachment) {
  if (!attachment) return false;

  const contentType = String(attachment.contentType || '').toLowerCase();
  if (contentType.startsWith('audio/')) return true;

  const name = String(attachment.name || attachment.url || '').toLowerCase();
  return AUDIO_FILE_RE.test(name);
}

function getAudioAttachmentFromOptions(interaction) {
  const attachment = interaction.options.getAttachment('audio', false);
  if (!attachment) return null;

  if (!isLikelyAudioAttachment(attachment)) {
    throw new Error(`"${attachment.name || 'plik'}" nie wyglada jak plik audio.`);
  }

  return attachment;
}

function buildPlayInputSelection(interaction) {
  const audioAttachment = getAudioAttachmentFromOptions(interaction);
  const query = String(interaction.options.getString('query', false) || '').trim();
  const hasAudio = Boolean(audioAttachment);
  const hasQuery = Boolean(query);

  if (!hasAudio && !hasQuery) {
    throw new Error('Podaj `query` albo plik `audio`.');
  }

  const selectedSources = Number(hasAudio) + Number(hasQuery);
  if (selectedSources > 1) {
    throw new Error('Wybierz jedno zrodlo: `query` albo `audio`.');
  }

  if (hasAudio) {
    return {
      kind: 'attachments',
      attachments: [audioAttachment],
      source: 'attachments',
    };
  }

  if (hasQuery) {
    return {
      kind: 'query',
      query,
      label: query,
      source: 'query',
    };
  }

  throw new Error('Nie udalo sie odczytac danych do odtwarzania.');
}

async function buildAttachmentQueueItems(_interaction, selection) {
  if (selection.kind === 'attachments') {
    return selection.attachments.map((attachment) => ({
      query: attachment.url,
      label: attachment.name || attachment.url,
      source: 'attachment',
    }));
  }

  return [];
}

async function resolveAttachmentQueueTracks({
  queueItems,
  musicManager,
  requestedBy,
  player,
  logger,
  guildId,
  userId,
}) {
  const resolvedTracks = [];
  const failedItems = [];

  for (const item of queueItems) {
    try {
      const resolved = await musicManager.resolveTracks(item.query, requestedBy, player);
      const track = Array.isArray(resolved?.tracks) ? resolved.tracks[0] : null;
      if (!track) {
        failedItems.push(item);
        continue;
      }
      resolvedTracks.push(track);
    } catch (error) {
      logger?.warn?.(
        `Music attachment resolve failed guild=${guildId} user=${userId} source=${item.source} label="${item.label}": ${error.message}`,
      );
      failedItems.push(item);
    }
  }

  if (resolvedTracks.length === 0) {
    throw new Error('Nie udalo sie odtworzyc zadnego pliku audio.');
  }

  return {
    resolvedTracks,
    failedItems,
  };
}

async function executeMusicCommand(interaction, { musicManager, logger }) {
  if (!interaction.inGuild()) {
    await interaction.reply({
      content: 'Ta komenda dziala tylko na serwerze.',
      ephemeral: true,
    });
    return;
  }

  const guild = interaction.guild;
  const guildId = guild.id;
  const mode = String(interaction.options.getString('mode', true) || '').toLowerCase();

  if (mode === 'play') {
    const voiceChannel = getMemberVoiceChannel(interaction);
    if (!voiceChannel) {
      await interaction.reply({
        content: 'Najpierw dolacz do kanalu glosowego.',
        ephemeral: true,
      });
      return;
    }

    if (voiceChannel.full) {
      await interaction.reply({
        content: 'Ten kanal glosowy jest pelny.',
        ephemeral: true,
      });
      return;
    }

    if (voiceChannel.joinable === false) {
      await interaction.reply({
        content: 'Bot nie moze dolaczyc do tego kanalu glosowego (joinable=false).',
        ephemeral: true,
      });
      return;
    }

    if (voiceChannel.speakable === false) {
      await interaction.reply({
        content: 'Bot nie moze nadawac na tym kanale glosowym (speakable=false).',
        ephemeral: true,
      });
      return;
    }

    const me = guild.members.me || (await guild.members.fetchMe().catch(() => null));
    if (!me) {
      await interaction.reply({
        content: 'Nie moge sprawdzic uprawnien bota.',
        ephemeral: true,
      });
      return;
    }

    const perms = voiceChannel.permissionsFor(me);
    if (!perms?.has(PermissionFlagsBits.Connect) || !perms?.has(PermissionFlagsBits.Speak)) {
      await interaction.reply({
        content: 'Bot nie ma uprawnien Connect i Speak na tym kanale.',
        ephemeral: true,
      });
      return;
    }

    let requestedQuery = '';
    let playStage = 'prepare';
    let playSession = null;
    let cleanupFreshSession = false;
    let playbackCommitted = false;
    try {
      const selection = buildPlayInputSelection(interaction);
      await interaction.deferReply();
      logger?.info?.(`Music command play start guild=${guildId} user=${interaction.user.id} source=${selection.source}`);

      playStage = 'connect';
      const session = await musicManager.ensureSession(guild, voiceChannel);
      playSession = session;
      cleanupFreshSession = session.createdPlayer === true;
      if (selection.kind === 'query') {
        const query = selection.query;
        requestedQuery = query;

        if (/open\.spotify\.com\/(playlist|album)/i.test(query)) {
          await interaction.editReply(
            'Rozpoznaje Spotify przez Lavalink. Przy duzych playlistach moze to chwile potrwac.',
          );
        }

        playStage = 'resolve';
        const resolved = await musicManager.resolveTracks(query, interaction.user, session.player);
        const tracks = Array.isArray(resolved?.tracks) ? resolved.tracks : [];
        logger?.info?.(
          `Music command play resolved guild=${guildId} user=${interaction.user.id} type=${resolved?.type || 'single'} count=${tracks.length} firstUrl="${tracks?.[0]?.url || ''}" firstTitle="${tracks?.[0]?.title || ''}"`,
        );
        if (tracks.length === 0) {
          throw new Error('Nie znaleziono utworu.');
        }
        if (resolved?.type === 'playlist') {
          playStage = 'enqueue';
          const batchResult = await musicManager.enqueueMany({
            guild,
            voiceChannel,
            requestedBy: interaction.user,
            tracks,
            session,
          });
          playbackCommitted = true;
          cleanupFreshSession = false;
          logger?.info?.(
            `Music command play playlist enqueued guild=${guildId} user=${interaction.user.id} added=${batchResult.added} started=${Boolean(batchResult.started)} startedTitle="${batchResult.startedTrack?.title || ''}"`,
          );

          const playlistTitle = String(resolved?.playlistTitle || 'Playlist');
          if (batchResult.started && batchResult.startedTrack) {
            await interaction.editReply(
              `Dodano playlistę "${playlistTitle}" (${batchResult.added} utworow). Teraz gra: ${formatTrackLine(batchResult.startedTrack)}.`,
            );
            return;
          }

          await interaction.editReply(
            `Dodano playlistę "${playlistTitle}" (${batchResult.added} utworow) do kolejki.`,
          );
          return;
        }

        playStage = 'enqueue';
        const result = await musicManager.enqueue({
          guild,
          voiceChannel,
          requestedBy: interaction.user,
          track: tracks[0],
          session,
        });
        playbackCommitted = true;
        cleanupFreshSession = false;
        logger?.info?.(
          `Music command play enqueued guild=${guildId} user=${interaction.user.id} started=${Boolean(result?.started)} trackUrl="${result?.track?.url || tracks?.[0]?.url || ''}" trackTitle="${result?.track?.title || tracks?.[0]?.title || ''}"`,
        );

        if (result.started) {
          await interaction.editReply(`Teraz gra: ${formatTrackLine(result.track)}.`);
          return;
        }

        await interaction.editReply(
          `Dodano do kolejki (pozycja ${result.position}): ${formatTrackLine(result.track)}.`,
        );
        return;
      }

      playStage = 'resolve';
      const queueItems = await buildAttachmentQueueItems(interaction, selection, logger);
      if (queueItems.length === 0) {
        throw new Error('Nie znaleziono plikow do dodania.');
      }
      requestedQuery = queueItems[0]?.query || '';

      const { resolvedTracks, failedItems } = await resolveAttachmentQueueTracks({
        queueItems,
        musicManager,
        requestedBy: interaction.user,
        player: session.player,
        logger,
        guildId,
        userId: interaction.user.id,
      });

      playStage = 'enqueue';
      const batchResult = await musicManager.enqueueMany({
        guild,
        voiceChannel,
        requestedBy: interaction.user,
        tracks: resolvedTracks,
        session,
      });
      playbackCommitted = true;
      cleanupFreshSession = false;
      logger?.info?.(
        `Music command batch enqueued guild=${guildId} user=${interaction.user.id} source=${selection.source} added=${batchResult.added} failed=${failedItems.length}`,
      );

      const sourceLabel = batchResult.added === 1 ? 'plik audio' : 'plikow audio';
      if (batchResult.started && batchResult.startedTrack) {
        await interaction.editReply(
          `Dodano ${batchResult.added} ${sourceLabel}. Teraz gra: ${formatTrackLine(batchResult.startedTrack)}.${failedItems.length > 0 ? ` Pominiete: ${failedItems.length}.` : ''}`,
        );
        return;
      }

      await interaction.editReply(
        `Dodano ${batchResult.added} ${sourceLabel} do kolejki.${failedItems.length > 0 ? ` Pominiete: ${failedItems.length}.` : ''}`,
      );
      return;
    } catch (error) {
      const freshSessionCleaned = (!playbackCommitted && cleanupFreshSession && playSession)
        ? await musicManager.cleanupFreshSession(playSession, `music play failed at ${playStage}`)
        : false;
      logger?.error?.(
        `Music command play error guild=${guildId} user=${interaction.user.id} stage=${playStage} source="${requestedQuery || 'n/a'}" cleanup=${freshSessionCleaned || error.musicFreshSessionCleaned ? 'fresh_session_destroyed' : 'none'}: ${error.stack || error.message}`,
      );

      if (!interaction.deferred && !interaction.replied) {
        await interaction.reply({
          content: `Blad odtwarzania: ${error.message}`,
          ephemeral: true,
        });
        return;
      }

      if (/no node|not connected|lavalink/i.test(String(error.message || ''))) {
        await interaction.editReply(
          'Blad odtwarzania: Lavalink nie jest polaczony albo nie obsluguje tego zrodla. Sprawdz node i pluginy.',
        );
        return;
      }

      if (/got 429|too many requests|rate.?limit/i.test(String(error.message || ''))) {
        await interaction.editReply(
          'Blad odtwarzania: zrodlo chwilowo blokuje zapytania (429/rate limit). Sprobuj ponownie za chwile albo wybierz inne zrodlo.',
        );
        return;
      }

      if (/operation was aborted/i.test(String(error.message || ''))) {
        await interaction.editReply(
          'Blad odtwarzania: The operation was aborted. Sprobuj ponownie za chwile lub podaj bezposredni link YouTube.',
        );
        return;
      }

      if (
        /nothing found/i.test(String(error.message || ''))
        && /(?:youtu\.be|youtube\.com)/i.test(String(requestedQuery || ''))
      ) {
        await interaction.editReply(
          'Blad odtwarzania: YouTube odrzucil ten link (wymaga logowania/anti-bot). Sprobuj inny link, fraze wyszukiwania albo SoundCloud.',
        );
        return;
      }

      if (
        /nothing found/i.test(String(error.message || ''))
        && /cdn\.discordapp\.com\/attachments\//i.test(String(requestedQuery || ''))
      ) {
        await interaction.editReply(
          'Blad odtwarzania: ten link do zalacznika Discord jest niedostepny lub wygasl. Wyslij plik ponownie i uzyj opcji `audio`.',
        );
        return;
      }

      if (/failed to retrieve secret from spotify|spotifytokentracker/i.test(String(error.message || ''))) {
        await interaction.editReply(
          'Blad odtwarzania: Spotify playlist/albums tymczasowo nie odpowiadaja po stronie API. Uzyj linku do pojedynczego utworu Spotify albo frazy wyszukiwania.',
        );
        return;
      }

      await interaction.editReply(`Blad odtwarzania: ${error.message}`);
      return;
    }
  }

  if (mode === 'queue') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot || (!snapshot.currentTrack && snapshot.queue.length === 0)) {
      await interaction.reply('Kolejka jest pusta.');
      return;
    }

    const lines = [];
    lines.push(`Kanal glosowy: <#${snapshot.voiceChannelId}>`);
    if (snapshot.currentTrack) {
      lines.push(`Teraz gra: ${formatTrackLine(snapshot.currentTrack)}`);
    }

    if (snapshot.queue.length > 0) {
      lines.push('Nastepne utwory:');
      const preview = snapshot.queue.slice(0, 10);
      for (let i = 0; i < preview.length; i += 1) {
        lines.push(formatTrackLine(preview[i], i + 1));
      }
      if (snapshot.queue.length > preview.length) {
        lines.push(`... i jeszcze ${snapshot.queue.length - preview.length}`);
      }
    }

    await interaction.reply(lines.join('\n'));
    return;
  }

  if (mode === 'nowplaying') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot || (!snapshot.currentTrack && snapshot.queue.length === 0)) {
      await interaction.reply({
        content: 'Nic teraz nie gra.',
        ephemeral: true,
      });
      return;
    }

    await interaction.reply(buildMusicPanelPayload(snapshot));
    return;
  }

  if (mode === 'skip') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot || !snapshot.currentTrack) {
      await interaction.reply({
        content: 'Nic nie gra.',
        ephemeral: true,
      });
      return;
    }

    if (!userIsInSameVoiceChannel(interaction, snapshot.voiceChannelId)) {
      await interaction.reply({
        content: 'Musisz byc na tym samym kanale glosowym co bot.',
        ephemeral: true,
      });
      return;
    }

    const skipped = await musicManager.skip(guildId);
    if (!skipped) {
      await interaction.reply({
        content: 'Nic nie gra.',
        ephemeral: true,
      });
      return;
    }

    await interaction.reply(`Pominieto: ${formatTrackLine(skipped)}.`);
    return;
  }

  if (mode === 'stop') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot) {
      await interaction.reply({
        content: 'Brak aktywnej sesji muzyki.',
        ephemeral: true,
      });
      return;
    }

    if (!userIsInSameVoiceChannel(interaction, snapshot.voiceChannelId)) {
      await interaction.reply({
        content: 'Musisz byc na tym samym kanale glosowym co bot.',
        ephemeral: true,
      });
      return;
    }

    await musicManager.stop(guildId);
    await interaction.reply('Zatrzymano muzyke i wyczyszczono kolejke.');
    return;
  }

  if (mode === 'pause') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot || !snapshot.currentTrack) {
      await interaction.reply({
        content: 'Nic nie gra.',
        ephemeral: true,
      });
      return;
    }

    const voiceError = requireSameVoiceChannel(interaction, snapshot.voiceChannelId);
    if (voiceError) {
      await interaction.reply({
        content: voiceError,
        ephemeral: true,
      });
      return;
    }

    if (snapshot.paused) {
      await interaction.reply({
        content: 'Muzyka jest juz wstrzymana.',
        ephemeral: true,
      });
      return;
    }

    const nextSnapshot = await musicManager.pause(guildId);
    if (!nextSnapshot) {
      await interaction.reply({
        content: 'Nic nie gra.',
        ephemeral: true,
      });
      return;
    }

    await interaction.reply(`Wstrzymano: ${formatTrackLine(nextSnapshot.currentTrack)}.`);
    return;
  }

  if (mode === 'resume') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot || !snapshot.currentTrack) {
      await interaction.reply({
        content: 'Nic nie gra.',
        ephemeral: true,
      });
      return;
    }

    const voiceError = requireSameVoiceChannel(interaction, snapshot.voiceChannelId);
    if (voiceError) {
      await interaction.reply({
        content: voiceError,
        ephemeral: true,
      });
      return;
    }

    if (!snapshot.paused) {
      await interaction.reply({
        content: 'Muzyka juz gra.',
        ephemeral: true,
      });
      return;
    }

    const nextSnapshot = await musicManager.resume(guildId);
    if (!nextSnapshot) {
      await interaction.reply({
        content: 'Brak aktywnej sesji muzyki.',
        ephemeral: true,
      });
      return;
    }

    await interaction.reply(`Wznowiono: ${formatTrackLine(nextSnapshot.currentTrack)}.`);
    return;
  }

  if (mode === 'volume') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot) {
      await interaction.reply({
        content: 'Brak aktywnej sesji muzyki.',
        ephemeral: true,
      });
      return;
    }

    const voiceError = requireSameVoiceChannel(interaction, snapshot.voiceChannelId);
    if (voiceError) {
      await interaction.reply({
        content: voiceError,
        ephemeral: true,
      });
      return;
    }

    const delta = interaction.options.getInteger('value', false);
    if (!Number.isInteger(delta) || delta === 0) {
      await interaction.reply({
        content: 'Dla mode=volume podaj `value`, np. `10` albo `-10`.',
        ephemeral: true,
      });
      return;
    }

    const nextSnapshot = await musicManager.adjustVolume(guildId, delta);
    if (!nextSnapshot) {
      await interaction.reply({
        content: 'Brak aktywnej sesji muzyki.',
        ephemeral: true,
      });
      return;
    }

    await interaction.reply(`Glosnosc ustawiona na **${nextSnapshot.volume}%** (zmiana ${delta > 0 ? '+' : ''}${delta}).`);
    return;
  }

  if (mode === 'remove') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot) {
      await interaction.reply({
        content: 'Brak aktywnej sesji muzyki.',
        ephemeral: true,
      });
      return;
    }

    const voiceError = requireSameVoiceChannel(interaction, snapshot.voiceChannelId);
    if (voiceError) {
      await interaction.reply({
        content: voiceError,
        ephemeral: true,
      });
      return;
    }

    const position = interaction.options.getInteger('position', false);
    if (!Number.isInteger(position) || position <= 0) {
      await interaction.reply({
        content: 'Dla mode=remove podaj `position` >= 1.',
        ephemeral: true,
      });
      return;
    }

    if (!snapshot.queue.length) {
      await interaction.reply({
        content: 'Kolejka jest pusta.',
        ephemeral: true,
      });
      return;
    }

    if (position > snapshot.queue.length) {
      await interaction.reply({
        content: `Nie ma utworu na pozycji ${position}. Kolejka ma teraz ${snapshot.queue.length} pozycji.`,
        ephemeral: true,
      });
      return;
    }

    const result = await musicManager.removeFromQueue(guildId, position);
    if (!result?.removed) {
      await interaction.reply({
        content: `Nie udalo sie usunac pozycji ${position} z kolejki.`,
        ephemeral: true,
      });
      return;
    }

    await interaction.reply(`Usunieto z kolejki (pozycja ${position}): ${formatTrackLine(result.removed)}.`);
    return;
  }

  if (mode === 'clear') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot) {
      await interaction.reply({
        content: 'Brak aktywnej sesji muzyki.',
        ephemeral: true,
      });
      return;
    }

    const voiceError = requireSameVoiceChannel(interaction, snapshot.voiceChannelId);
    if (voiceError) {
      await interaction.reply({
        content: voiceError,
        ephemeral: true,
      });
      return;
    }

    if (!snapshot.queue.length) {
      await interaction.reply({
        content: 'Kolejka jest juz pusta.',
        ephemeral: true,
      });
      return;
    }

    const result = await musicManager.clearQueue(guildId);
    await interaction.reply(`Wyczyszczono kolejke. Usunieto **${result?.removed || 0}** oczekujacych utworow.`);
    return;
  }

  if (mode === 'reconnect') {
    const snapshot = musicManager.getQueueSnapshot(guildId);
    if (!snapshot) {
      await interaction.reply({
        content: 'Brak aktywnej sesji muzyki.',
        ephemeral: true,
      });
      return;
    }

    const voiceError = requireSameVoiceChannel(interaction, snapshot.voiceChannelId);
    if (voiceError) {
      await interaction.reply({
        content: voiceError,
        ephemeral: true,
      });
      return;
    }

    try {
      await interaction.deferReply({ ephemeral: true });
      const result = await musicManager.reconnectSession(guildId);
      if (!result?.snapshot) {
        await interaction.editReply('Brak aktywnej sesji muzyki.');
        return;
      }

      await interaction.editReply(
        `Reconnect zakonczony. Kanal: <#${result.voiceChannelId}>. Node: **${result.nodeConnected ? 'online' : 'offline'}**.`,
      );
      return;
    } catch (error) {
      logger?.warn?.(`Music reconnect error guild=${guildId} user=${interaction.user.id}: ${error.message}`);
      if (!interaction.deferred && !interaction.replied) {
        await interaction.reply({
          content: `Reconnect nie powiodl sie: ${error.message}`,
          ephemeral: true,
        });
        return;
      }
      await interaction.editReply(`Reconnect nie powiodl sie: ${error.message}`);
      return;
    }
  }

  await interaction.reply({
    content: `Nieznany mode music: ${mode}.`,
    ephemeral: true,
  });
}

async function executeMusicButtonInteraction(interaction, { musicManager, logger }) {
  const parsed = parseMusicComponentId(interaction.customId);
  if (!parsed) return false;

  if (!interaction.inGuild() || interaction.guildId !== parsed.guildId) {
    await interaction.reply({
      content: 'Ten panel nie pasuje do tego serwera.',
      ephemeral: true,
    });
    return true;
  }

  try {
    const snapshot = musicManager.getQueueSnapshot(interaction.guildId);
    const memberVoice = getMemberVoiceChannel(interaction);
    const action = parsed.action;

    if (parsed.kind === 'button' && action === 'restorequeue' && !memberVoice) {
      await interaction.reply({
        content: 'Dolacz do kanalu glosowego, zeby przywrocic kolejke.',
        ephemeral: true,
      });
      return true;
    }

    if (!snapshot && !(parsed.kind === 'button' && action === 'restorequeue')) {
      await interaction.update(buildMusicPanelPayload(null));
      return true;
    }

    if (snapshot && !userIsInSameVoiceChannel(interaction, snapshot.voiceChannelId)) {
      await interaction.reply({
        content: 'Musisz byc na tym samym kanale glosowym co bot.',
        ephemeral: true,
      });
      return true;
    }

    if (parsed.kind === 'button' && action === 'queue') {
      await interaction.reply({
        content: buildQueuePreviewContent(snapshot),
        ephemeral: true,
      });
      return true;
    }

    if (parsed.kind === 'button' && action === 'lyrics') {
      await interaction.reply({
        content: buildLyricsContent(snapshot),
        ephemeral: true,
      });
      return true;
    }

    if (parsed.kind === 'button' && action === 'eq') {
      await interaction.reply({
        content: 'Wybierz preset z menu EQ ponizej.',
        ephemeral: true,
      });
      return true;
    }

    if (parsed.kind === 'button' && action === 'fx') {
      await interaction.reply({
        content: 'Wybierz efekt z menu FX ponizej.',
        ephemeral: true,
      });
      return true;
    }

    await interaction.deferUpdate();
    let followUpMessage = null;

    if (parsed.kind === 'select' && action === 'eq') {
      const preset = String(interaction.values?.[0] || '');
      await musicManager.applyEqPreset(interaction.guildId, preset);
    } else if (parsed.kind === 'select' && action === 'fx') {
      const effect = String(interaction.values?.[0] || '');
      await musicManager.toggleFx(interaction.guildId, effect);
    } else if (action === 'loop') {
      await musicManager.cycleLoop(interaction.guildId);
    } else if (action === 'bassboost') {
      await musicManager.toggleBassboost(interaction.guildId);
    } else if (action === 'volumedown') {
      await musicManager.adjustVolume(interaction.guildId, -10);
    } else if (action === 'volumeup') {
      await musicManager.adjustVolume(interaction.guildId, 10);
    } else if (action === 'mute') {
      await musicManager.toggleMute(interaction.guildId);
    } else if (action === 'playpause') {
      await musicManager.togglePause(interaction.guildId);
    } else if (action === 'stop') {
      await musicManager.stop(interaction.guildId);
    } else if (action === 'shuffle') {
      await musicManager.shuffle(interaction.guildId);
    } else if (action === 'vaporwave') {
      await musicManager.toggleVaporwave(interaction.guildId);
    } else if (action === 'autoplay') {
      const enabled = musicManager.toggleAutoplay(interaction.guildId);
      followUpMessage = `Autoplay: **${enabled ? 'ON' : 'OFF'}**`;
    } else if (action === 'savequeue') {
      const saved = musicManager.saveQueueState(interaction.guildId);
      followUpMessage = `Zapisano kolejke (${saved.tracksSaved} utworow).`;
    } else if (action === 'restorequeue') {
      const restored = await musicManager.restoreQueueState(interaction.guild, memberVoice);
      followUpMessage = `Przywrocono kolejke: ${restored.restored} utworow.`;
    } else if (action === 'refresh') {
      // no-op
    }

    const nextSnapshot = musicManager.getQueueSnapshot(interaction.guildId);
    await interaction.editReply(buildMusicPanelPayload(nextSnapshot));

    if (followUpMessage) {
      await interaction.followUp({
        content: followUpMessage,
        ephemeral: true,
      }).catch(() => {});
    }
  } catch (error) {
    logger?.error?.(`Music button error guild=${interaction.guildId} action=${parsed.action}: ${error.stack || error.message}`);
    if (!interaction.replied && !interaction.deferred) {
      await interaction.reply({
        content: `Blad panelu muzyki: ${error.message}`,
        ephemeral: true,
      });
      return true;
    }
    await interaction.followUp({
      content: `Blad panelu muzyki: ${error.message}`,
      ephemeral: true,
    }).catch(() => {});
  }

  return true;
}

module.exports = {
  musicCommand,
  executeMusicCommand,
  executeMusicButtonInteraction,
};
