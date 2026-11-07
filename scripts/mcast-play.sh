#!/usr/bin/env bash
# mcast-play.sh - stream an audio file to a multicast paging group, tuned for the content and codec.
#
# Usage: ./mcast-play.sh [options] <audio-file>      (options can go before or after the file)
#   -c CODEC   g711u (default), g711a, g722, opus, opuswb
#   -m MODE    voice | music | none (default none)  - processing tuned for the content and codec
#   -p         shortcut for -m voice
#   -v DB      volume change in dB, e.g. -v -6 (quieter) or -v 3 (louder)
#   -C TONE    pre-tone before the announcement: "default", or a path to your own audio file
#   -G MS      gap between the pre-tone and the announcement, in ms (default 400)
#   -L MS      dead air before everything, in ms, so the start isn't clipped (default 0; try 800-1000)
#   -E MS      dead air after the audio, in ms, so the end isn't cut off (default 300)
#   -g GROUP   multicast group (default 239.192.2.1)
#   -P PORT    UDP port (default 10000)
#   -t SECS    play only SECS seconds of the file (adds a short fade-out)
#   -s SECS    start SECS seconds into the file (adds a short fade-in)
#   -l         loop until you press q
#   -T PT      payload type for Opus (default 107)
#   -o FILE    render to a WAV of exactly what the phone would decode, instead of streaming
#   -n         dry run: print the ffmpeg command(s), don't run them
#
# Pre-tone: "-C default" plays DEFAULT_TONE below if you set it to a file, otherwise a built-in
# three-note chime. "-C some/file.wav" plays that file instead. The tone is level-matched and
# band-limited for the codec, but not voice-processed, so it stays clean.
#
# Examples:
#   ./mcast-play.sh rasp1.wav                          # plain G.711u to 239.192.2.1
#   ./mcast-play.sh rasp1.wav -p -L 1000 -E 1000       # voice-tuned G.711u, 1 s dead air each side
#   ./mcast-play.sh rasp1.wav -p -C default -L 1000 -E 1000   # same, with the default chime first
#   ./mcast-play.sh rasp1.wav -p -C tones/bell.wav -G 600     # your own tone, 0.6 s gap after it
#   ./mcast-play.sh song.flac -c g722 -m music -v -6   # music-tuned G.722, 6 dB quieter
#   ./mcast-play.sh rasp1.wav -p -C default -o preview.wav    # hear it on your desktop first

VERSION="1.8"
DEFAULT_TONE=""   # set to an audio file path to make it the "-C default" tone; empty = built-in chime

CODEC="g711u"; MODE="none"; GROUP="239.192.2.1"; PORT="10000"; DUR=""; START=""
LOOP=""; OPUS_PT="107"; DRY=""; VOL=""; LEAD="0"; TAIL="300"; OUT=""; TONE=""; GAP="400"

usage() {  # print the comment header only, however long it gets
  awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
  echo "  (version $VERSION)"; exit 1
}
die()   { echo "$*" >&2; exit 1; }
isnum() { [[ "$1" =~ ^[+-]?[0-9]+(\.[0-9]+)?$ ]]; }

# --- Parse options (before or after the filename) ---
POSITIONAL=()
while [[ $# -gt 0 ]]; do
  OPTIND=1
  while getopts "c:m:pv:C:G:L:E:g:P:t:s:lT:o:nh" opt; do
    case "$opt" in
      c) CODEC="$OPTARG" ;;  m) MODE="$OPTARG" ;;   p) MODE="voice" ;;
      v) VOL="$OPTARG" ;;    C) TONE="$OPTARG" ;;   G) GAP="$OPTARG" ;;
      L) LEAD="$OPTARG" ;;   E) TAIL="$OPTARG" ;;   g) GROUP="$OPTARG" ;;
      P) PORT="$OPTARG" ;;   t) DUR="$OPTARG" ;;    s) START="$OPTARG" ;;
      l) LOOP=1 ;;           T) OPUS_PT="$OPTARG" ;; o) OUT="$OPTARG" ;;
      n) DRY=1 ;;            *) usage ;;
    esac
  done
  shift $((OPTIND - 1))
  [[ $# -gt 0 ]] && { POSITIONAL+=("$1"); shift; }
done
[[ ${#POSITIONAL[@]} -ne 1 ]] && { echo "Expected exactly one audio file, got: ${POSITIONAL[*]:-none}" >&2; usage; }
FILE="${POSITIONAL[0]}"
[[ -f "$FILE" ]] || die "File not found: $FILE"
[[ -n "$VOL" ]] && ! isnum "$VOL" && die "Volume must be a number of dB, e.g. -6 or 3: $VOL"
for pair in "L:$LEAD" "E:$TAIL" "G:$GAP"; do isnum "${pair#*:}" || die "-${pair%%:*} must be a number of ms: ${pair#*:}"; done
[[ -n "$DUR" ]]   && ! isnum "$DUR"   && die "-t must be a number of seconds: $DUR"
[[ -n "$START" ]] && ! isnum "$START" && die "-s must be a number of seconds: $START"
[[ -n "$OUT" && -n "$LOOP" ]] && die "-o and -l can't be combined (the render would never end)"
case "$MODE" in voice|music|none) ;; *) die "Unknown mode: $MODE (use voice, music, none)" ;; esac

# --- Pre-tone source ---
TONE_FILE=""; TONE_DESC=""
if [[ -n "$TONE" ]]; then
  if [[ "$TONE" == default ]]; then
    if [[ -n "$DEFAULT_TONE" ]]; then
      [[ -f "$DEFAULT_TONE" ]] || die "DEFAULT_TONE file not found: $DEFAULT_TONE (edit the top of the script)"
      TONE_FILE="$DEFAULT_TONE"; TONE_DESC="tone $(basename "$DEFAULT_TONE")"
    else
      TONE_DESC="tone built-in"
    fi
  else
    [[ -f "$TONE" ]] || die "Tone file not found: $TONE (use -C default for the built-in chime)"
    TONE_FILE="$TONE"; TONE_DESC="tone $(basename "$TONE")"
  fi
fi
# Built-in chime: three ascending bell notes (C5, E5, G5) with a soft overtone and natural decay.
# All energy sits in 500-1600 Hz, so it sounds the same on G.711 as on the wideband codecs.
note() { echo "if(gte(t,$2),(0.30*sin(2*PI*$1*(t-$2))+0.08*sin(2*PI*$1*2*(t-$2)))*exp(-3.5*(t-$2)),0)"; }
CHIME_EXPR="$(note 523.25 0)+$(note 659.25 0.35)+$(note 783.99 0.70)"
CHIME_SRC=(-f lavfi -i "aevalsrc='${CHIME_EXPR}':s=48000:d=2.0")

# --- Codec settings ---
# BAND: narrow (G.711) or wide; LP: low-pass for the codec's band; RAW: raw format for -o renders
case "$CODEC" in
  g711u)  ENC=(-acodec pcm_mulaw -ar 8000);  BAND=narrow; LP=3400; RAW=(-f mulaw -ar 8000) ;;
  g711a)  ENC=(-acodec pcm_alaw  -ar 8000);  BAND=narrow; LP=3400; RAW=(-f alaw  -ar 8000) ;;
  g722)   ENC=(-acodec g722      -ar 16000); BAND=wide;   LP=7000; RAW=(-f g722) ;;
  opuswb) ENC=(-acodec libopus -ar 16000 -b:a 32k); BAND=wide; LP=7000; RAW=(-f ogg) ;;
  opus)   ENC=(-acodec libopus -ar 48000 -b:a 64k); BAND=wide; LP="";   RAW=(-f ogg) ;;
  *) die "Unknown codec: $CODEC (use g711u, g711a, g722, opus, opuswb)" ;;
esac
if [[ "$CODEC" == opus* ]]; then
  # Opus's own mode follows the content: voip favors speech, audio favors music
  if   [[ "$MODE" == voice ]];   then APP=voip
  elif [[ "$MODE" == music ]];   then APP=audio
  elif [[ "$CODEC" == opuswb ]]; then APP=voip
  else APP=audio; fi
  # In-band FEC lets the phone conceal a lost packet from the next one
  # (":a" scopes it to the encoder; plain -fec collides with the RTP muxer's own fec option)
  ENC+=(-application "$APP" -frame_duration 20 -vbr on -fec:a 1 -packet_loss:a 5)
fi

# Downmix to mono first, by averaging. ffmpeg's default stereo-to-mono mix sums the channels
# at -3 dB each, which makes typical (near-identical) L/R 3 dB louder; done after loudnorm, that
# pushed true peak over 0 dBFS. Doing it first means everything is measured as it will be sent.
MONO="aresample=ochl=mono:rematrix_maxval=1"

# --- Announcement filter chain ---
FX=("$MONO")
[[ -n "$START" ]] && FX+=("afade=t=in:d=0.05")
[[ -n "$DUR" ]]   && FX+=("afade=t=out:st=$(awk "BEGIN{print ($DUR>0.3)?$DUR-0.3:0}"):d=0.3")
if [[ "$MODE" == voice ]]; then
  # Rumble/plosive cut, light denoise, steady level, tame sibilance, presence lift
  if [[ $BAND == narrow ]]; then HP=250; EQ="equalizer=f=2500:t=q:w=1:g=3"
  else                           HP=120; EQ="equalizer=f=3000:t=q:w=1:g=2"; fi
  FX+=("highpass=f=${HP}" "afftdn=nr=10:nf=-40"
       "acompressor=threshold=-20dB:ratio=4:attack=5:release=120:makeup=3"
       "deesser=i=0.4" "$EQ")
  LOUDNORM="loudnorm=I=-16:TP=-2:LRA=6"
elif [[ "$MODE" == music ]]; then
  # Keep more low end and dynamics; on G.711, lean on the midrange that survives
  if [[ $BAND == narrow ]]; then HP=150; EQ="equalizer=f=300:t=q:w=1:g=-2,equalizer=f=1800:t=q:w=1.5:g=2"
  else                           HP=60;  EQ=""; fi
  FX+=("highpass=f=${HP}" "acompressor=threshold=-24dB:ratio=2:attack=20:release=250:makeup=2")
  [[ -n "$EQ" ]] && FX+=("$EQ")
  LOUDNORM="loudnorm=I=-18:TP=-2:LRA=9"
fi
# Band-limit before loudnorm, not after: filtering after its peak limiter can push peaks back over
if [[ "$MODE" != none ]]; then
  [[ -n "$LP" ]] && FX+=("lowpass=f=${LP}")
  FX+=("$LOUDNORM")
fi
NORM="aresample=48000:resampler=soxr:precision=28,aformat=sample_fmts=fltp:channel_layouts=mono"
FX+=("$NORM")
AF_MAIN=$(IFS=,; echo "${FX[*]}")

# --- Final chain: (pre-tone + gap) + announcement, then volume and dead air ---
POST=()
# Volume after processing, so loudnorm can't undo it; limiter stops boosts from clipping
[[ -n "$VOL" ]] && POST+=("volume=${VOL}dB" "alimiter=limit=0.89:level=disabled")
# Dead air before and after: real silent packets, so the page is already open when audio starts
# and stays open until the audio has fully played out
[[ "$LEAD" != 0 ]] && POST+=("adelay=${LEAD}:all=1")
[[ "$TAIL" != 0 ]] && POST+=("apad=pad_dur=${TAIL}ms")
POST+=("$NORM")
AF_POST=$(IFS=,; echo "${POST[*]}")

IN=()
[[ -n "$START" ]] && IN+=(-ss "$START")
[[ -n "$DUR" ]]   && IN+=(-t "$DUR")
BASE=(ffmpeg -hide_banner -loglevel error)

if [[ -n "$TONE" ]]; then
  # Tone: mono, band-limited for the codec, level-matched near the announcement, then the gap.
  # Not voice-processed: denoise and de-essing would smear a bell.
  TFX="${MONO},highpass=f=200${LP:+,lowpass=f=${LP}},loudnorm=I=-18:TP=-3,${NORM},apad=pad_dur=${GAP}ms"
  if [[ -n "$TONE_FILE" ]]; then TIN=(-i "$TONE_FILE"); else TIN=("${CHIME_SRC[@]}"); fi
  GRAPH="[0:a]${TFX}[tone];[1:a]${AF_MAIN}[main];[tone][main]concat=n=2:v=0:a=1,${AF_POST}[out]"
  PREP_IN=("${TIN[@]}" "${IN[@]}" -i "$FILE")
else
  GRAPH="[0:a]${AF_MAIN},${AF_POST}[out]"
  PREP_IN=("${IN[@]}" -i "$FILE")
fi

DESC="v${VERSION} | $(basename "$FILE") | ${CODEC} | ${MODE}${VOL:+ | ${VOL} dB}${TONE_DESC:+ | ${TONE_DESC}}"
[[ "$LEAD" != 0 || "$TAIL" != 300 ]] && DESC+=" | dead air ${LEAD}/${TAIL} ms"

# Stage 1 for both paths: build the finished audio (tone, processing, dead air) into a temp WAV,
# so heavy filters like loudnorm can't stall the stream while it's being sent.
TMPWAV=$(mktemp --suffix=".wav"); TMP=""
trap 'rm -f "$TMPWAV" "$TMP"' EXIT
PREP=("${BASE[@]}" -y "${PREP_IN[@]}" -filter_complex "$GRAPH" -map "[out]" -acodec pcm_s16le -ar 48000 "$TMPWAV")

# Stage 2: resample to the encoder's rate and cut exactly one 20 ms packet per frame
# (160 samples at 8 kHz, 320 at 16 kHz, 960 at 48 kHz)
case "$CODEC" in
  g711*)       RATE=8000 ;;
  g722|opuswb) RATE=16000 ;;
  *)           RATE=48000 ;;
esac
AF2="aresample=${RATE}:resampler=soxr:precision=28,asetnsamples=n=$((RATE / 50)):p=1"

if [[ -n "$OUT" ]]; then
  # Render: encode with the real codec, then decode back to WAV, so you hear the codec's artifacts
  TMP=$(mktemp --suffix=".enc")
  ENCODE=("${BASE[@]}" -y -i "$TMPWAV" -af "$AF2" "${ENC[@]}" "${RAW[@]}" "$TMP")
  if [[ "$CODEC" == g711* ]]; then DECODE=("${BASE[@]}" -y "${RAW[@]}" -ac 1 -i "$TMP" -acodec pcm_s16le "$OUT")
  else                             DECODE=("${BASE[@]}" -y "${RAW[@]}" -i "$TMP" -ac 1 -acodec pcm_s16le "$OUT"); fi
  echo ">>> $DESC | rendering to $OUT"
  if [[ -n "$DRY" ]]; then
    for c in PREP ENCODE DECODE; do declare -n a=$c; printf '%q ' "${a[@]}"; echo; done
  else
    "${PREP[@]}" && "${ENCODE[@]}" && "${DECODE[@]}"
  fi
else
  # dscp=46 (EF) is TOS 184; ffmpeg's rtp protocol has no "tos" option, so tos=184 was ignored
  URL="rtp://${GROUP}:${PORT}?ttl=16&dscp=46"
  [[ "$CODEC" == opus* ]] && ENC+=(-payload_type "$OPUS_PT") || URL+="&pkt_size=172"
  LOOPIN=(); [[ -n "$LOOP" ]] && LOOPIN=(-stream_loop -1)
  # Pace the output, not the input: arealtime releases each 20 ms frame (one packet) on time.
  # -re only throttles reading the file, so packets left in bursts (~7 every ~140 ms).
  CMD=("${BASE[@]}" -stats "${LOOPIN[@]}" -i "$TMPWAV" -af "$AF2,arealtime" "${ENC[@]}" -f rtp "$URL")
  echo ">>> $DESC | ${GROUP}:${PORT}${LOOP:+ | looping, q to stop}"
  if [[ -n "$DRY" ]]; then
    printf '%q ' "${PREP[@]}"; echo; printf '%q ' "${CMD[@]}"; echo
  else
    "${PREP[@]}" || exit $?
    "${CMD[@]}"
  fi
fi
