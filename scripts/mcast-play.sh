#!/usr/bin/env bash
# mcast-play.sh - stream an audio file to a multicast paging group, tuned for the content and codec.
#
# Usage: ./mcast-play.sh [options] <audio-file>      (options can go before or after the file)
#   -c CODEC   g711u (default), g711a, g722, opus, opuswb
#   -m MODE    voice | music | none (default none)  - processing tuned for the content and codec
#   -p         shortcut for -m voice
#   -v DB      volume change in dB, e.g. -v -6 (quieter) or -v 3 (louder)
#   -L MS      lead-in silence in ms, so the first word isn't clipped (default 0)
#   -g GROUP   multicast group (default 239.192.2.1)
#   -P PORT    UDP port (default 10000)
#   -t SECS    stop after SECS seconds (adds a short fade-out)
#   -s SECS    start SECS seconds into the file (adds a short fade-in)
#   -l         loop until you press q
#   -T PT      payload type for Opus (default 107)
#   -o FILE    render to a WAV of exactly what the phone would decode, instead of streaming
#   -n         dry run: print the ffmpeg command(s), don't run them
#
# Examples:
#   ./mcast-play.sh rasp1.wav                          # plain G.711u to 239.192.2.1
#   ./mcast-play.sh rasp1.wav -m voice -L 500          # voice-tuned G.711u, half-second lead-in
#   ./mcast-play.sh song.flac -c g722 -m music -v -6   # music-tuned G.722, 6 dB quieter
#   ./mcast-play.sh song.flac -c g711u -m music -o preview.wav   # hear it on your desktop first

CODEC="g711u"; MODE="none"; GROUP="239.192.2.1"; PORT="10000"; DUR=""; START=""
LOOP=""; OPUS_PT="107"; DRY=""; VOL=""; LEAD="0"; OUT=""

usage() { sed -n '2,26p' "$0"; exit 1; }
die()   { echo "$*" >&2; exit 1; }
isnum() { [[ "$1" =~ ^[+-]?[0-9]+(\.[0-9]+)?$ ]]; }

# --- Parse options (before or after the filename) ---
POSITIONAL=()
while [[ $# -gt 0 ]]; do
  OPTIND=1
  while getopts "c:m:pv:L:g:P:t:s:lT:o:nh" opt; do
    case "$opt" in
      c) CODEC="$OPTARG" ;;  m) MODE="$OPTARG" ;;   p) MODE="voice" ;;
      v) VOL="$OPTARG" ;;    L) LEAD="$OPTARG" ;;   g) GROUP="$OPTARG" ;;
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
isnum "$LEAD" || die "Lead-in must be a number of ms: $LEAD"
[[ -n "$DUR" ]]   && ! isnum "$DUR"   && die "-t must be a number of seconds: $DUR"
[[ -n "$START" ]] && ! isnum "$START" && die "-s must be a number of seconds: $START"
[[ -n "$OUT" && -n "$LOOP" ]] && die "-o and -l can't be combined (the render would never end)"
case "$MODE" in voice|music|none) ;; *) die "Unknown mode: $MODE (use voice, music, none)" ;; esac

# --- Codec settings ---
# BAND: narrow (G.711) or wide; LP: low-pass for the codec's band; RAW: raw format for -o renders
# RATE: the encoder's sample rate (used to cut audio into one-packet frames when streaming)
case "$CODEC" in
  g711u)  ENC=(-acodec pcm_mulaw -ar 8000);  BAND=narrow; LP=3400; RAW=(-f mulaw -ar 8000); RATE=8000 ;;
  g711a)  ENC=(-acodec pcm_alaw  -ar 8000);  BAND=narrow; LP=3400; RAW=(-f alaw  -ar 8000); RATE=8000 ;;
  g722)   ENC=(-acodec g722      -ar 16000); BAND=wide;   LP=7000; RAW=(-f g722); RATE=16000 ;;
  opuswb) ENC=(-acodec libopus -ar 16000 -b:a 32k); BAND=wide; LP=7000; RAW=(-f ogg); RATE=16000 ;;
  opus)   ENC=(-acodec libopus -ar 48000 -b:a 64k); BAND=wide; LP="";   RAW=(-f ogg); RATE=48000 ;;
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

# --- Filter chain ---
FX=()
[[ -n "$START" ]] && FX+=("afade=t=in:d=0.05")
[[ -n "$DUR" ]]   && FX+=("afade=t=out:st=$(awk "BEGIN{print ($DUR>0.3)?$DUR-0.3:0}"):d=0.3")

if [[ "$MODE" == voice ]]; then
  # Rumble/plosive cut, light denoise, steady level, tame sibilance, presence lift
  if [[ $BAND == narrow ]]; then HP=250; EQ="equalizer=f=2500:t=q:w=1:g=3"
  else                           HP=120; EQ="equalizer=f=3000:t=q:w=1:g=2"; fi
  FX+=("highpass=f=${HP}" "afftdn=nr=10:nf=-40"
       "acompressor=threshold=-20dB:ratio=4:attack=5:release=120:makeup=3"
       "deesser=i=0.4" "$EQ" "loudnorm=I=-16:TP=-2:LRA=6")
elif [[ "$MODE" == music ]]; then
  # Keep more low end and dynamics; on G.711, lean on the midrange that survives
  if [[ $BAND == narrow ]]; then HP=150; EQ="equalizer=f=300:t=q:w=1:g=-2,equalizer=f=1800:t=q:w=1.5:g=2"
  else                           HP=60;  EQ=""; fi
  FX+=("highpass=f=${HP}" "acompressor=threshold=-24dB:ratio=2:attack=20:release=250:makeup=2")
  [[ -n "$EQ" ]] && FX+=("$EQ")
  FX+=("loudnorm=I=-18:TP=-2:LRA=9")
fi
[[ "$MODE" != none && -n "$LP" ]] && FX+=("lowpass=f=${LP}")

# Volume after processing, so loudnorm can't undo it; limiter stops boosts from clipping
[[ -n "$VOL" ]] && FX+=("volume=${VOL}dB" "alimiter=limit=0.89:level=disabled")
# Lead-in silence, and a short tail so the last word isn't cut off
[[ "$LEAD" != 0 ]] && FX+=("adelay=${LEAD}:all=1")
FX+=("apad=pad_dur=0.3" "aresample=resampler=soxr:precision=28")
AF=$(IFS=,; echo "${FX[*]}")

# --- Build command(s) ---
IN=()
[[ -n "$LOOP" ]]  && IN+=(-stream_loop -1)
[[ -n "$START" ]] && IN+=(-ss "$START")
[[ -n "$DUR" ]]   && IN+=(-t "$DUR")
BASE=(ffmpeg -hide_banner -loglevel error)

DESC="$(basename "$FILE") | ${CODEC} | ${MODE}${VOL:+ | ${VOL} dB}"
[[ "$LEAD" != 0 ]] && DESC+=" | ${LEAD} ms lead-in"

if [[ -n "$OUT" ]]; then
  # Render: encode with the real codec, then decode back to WAV, so you hear the codec's artifacts
  TMP=$(mktemp --suffix=".enc")
  ENCODE=("${BASE[@]}" -y "${IN[@]}" -i "$FILE" -ac 1 -af "$AF" "${ENC[@]}" "${RAW[@]}" "$TMP")
  DECODE=("${BASE[@]}" -y "${RAW[@]}" -i "$TMP" -ac 1 -acodec pcm_s16le "$OUT")
  [[ "$CODEC" == g711* ]] && DECODE=("${BASE[@]}" -y "${RAW[@]}" -ac 1 -i "$TMP" -acodec pcm_s16le "$OUT")
  echo ">>> $DESC | rendering to $OUT"
  if [[ -n "$DRY" ]]; then
    printf '%q ' "${ENCODE[@]}"; echo; printf '%q ' "${DECODE[@]}"; echo; rm -f "$TMP"
  else
    "${ENCODE[@]}" && "${DECODE[@]}"; rc=$?; rm -f "$TMP"; exit $rc
  fi
else
  # dscp=46 (EF) is TOS 184. ffmpeg's rtp protocol has no "tos" option, so tos=184 was ignored.
  URL="rtp://${GROUP}:${PORT}?ttl=16&dscp=46"
  [[ "$CODEC" == opus* ]] && ENC+=(-payload_type "$OPUS_PT") || URL+="&pkt_size=172"
  # Pace the output, not the input. -re only throttles reading the file, so packets
  # left in bursts (~7 every ~140 ms), which phones' jitter buffers handle badly.
  # Two ffmpegs joined by a pipe: the first processes as fast as it can into raw PCM at
  # the encoder's rate; the second cuts it into one 20 ms frame per packet and releases
  # frames in real time. The pipe's buffer absorbs processing hiccups (loudnorm stalls
  # ~80 ms when its 3 s window fills), which would otherwise delay packets and bunch them up.
  # The sender reads the audio on fd 3 so it keeps the terminal: q stops it at once
  # (rather than after the buffered audio plays out), and the processor then exits too.
  PROCESS=("${BASE[@]}" -nostdin -nostats "${IN[@]}" -i "$FILE" -ac 1 -af "$AF,aformat=sample_rates=${RATE}:channel_layouts=mono" -f s16le -)
  SEND=("${BASE[@]}" -stats -f s16le -ar "$RATE" -ac 1 -i /dev/fd/3 -af "asetnsamples=n=$((RATE / 50)),arealtime" "${ENC[@]}" -f rtp "$URL")
  echo ">>> $DESC | ${GROUP}:${PORT}${LOOP:+ | looping, q to stop}"
  if [[ -n "$DRY" ]]; then
    printf '%q ' "${SEND[@]}"; printf '3< <('; printf '%q ' "${PROCESS[@]}"; echo ')'
  else
    "${SEND[@]}" 3< <("${PROCESS[@]}"); rc=$?
    wait $! 2>/dev/null   # let the processor finish (it exits once the sender stops)
    exit $rc
  fi
fi
