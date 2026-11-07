#!/usr/bin/env bash
# mcast-play.sh - stream an audio file to a multicast paging group, tuned for the content and codec.
#
# Usage: ./mcast-play.sh [options] <audio-file|pad>  (options can go before or after the file)
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
# pad: in place of a file, a soft, slowly changing chord to test with (the same as
# "mcast audio send --tone pad"): 64 s, or -t SECS, or -l to loop it seamlessly. Peaks at
# -12 dBFS before -v. To play a file named "pad", give it as ./pad.
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
#   ./mcast-play.sh pad -l                             # test pad on G.711u until you press q
#   ./mcast-play.sh pad -c g722 -t 120 -o pad.wav      # two minutes of it through G.722
#
# Presets: save options under a name, then use them with @NAME (before or after the file).
# Options given on the command line win; several presets apply left to right.
#   ./mcast-play.sh preset create NAME [options]   new preset
#   ./mcast-play.sh preset set NAME options        add or change options (creates it if needed)
#   ./mcast-play.sh preset unset NAME OPT...       remove options, by letter: C or -C
#   ./mcast-play.sh preset list [NAME]             show presets (also: ls, show)
#   ./mcast-play.sh preset copy FROM TO            (also: cp)
#   ./mcast-play.sh preset delete NAME             (also: rm)
#   ./mcast-play.sh preset create page -p -C default -L 1000 -E 1000
#   ./mcast-play.sh @page rasp1.wav                # same as: rasp1.wav -p -C default -L 1000 -E 1000
#   ./mcast-play.sh @page rasp1.wav -c g722 -g 239.192.2.2
# Presets are files in ~/.config/mcast/play-presets/ ($MCAST_PLAY_PRESETS overrides), one
# option per line, so they can be edited by hand.

VERSION="1.10"
DEFAULT_TONE=""   # set to an audio file path to make it the "-C default" tone; empty = built-in chime


usage() {  # print the comment header only, however long it gets
  awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
  echo "  (version $VERSION)"; exit 1
}
die()   { echo "$*" >&2; exit 1; }
isnum() { [[ "$1" =~ ^[+-]?[0-9]+(\.[0-9]+)?$ ]]; }

# --- Parse options (before or after the filename) ---
# Sets the option variables, POSITIONAL (non-options), and SEEN: each option given, as
# "-X" or "-X value", for saving presets.
OPTSTRING="c:m:pv:C:G:L:E:g:P:t:s:lT:o:nh"
parse_opts() {
  CODEC="g711u"; MODE="none"; GROUP="239.192.2.1"; PORT="10000"; DUR=""; START=""
  LOOP=""; OPUS_PT="107"; DRY=""; VOL=""; LEAD="0"; TAIL="300"; OUT=""; TONE=""; GAP="400"
  POSITIONAL=(); SEEN=()
  local opt
  while [[ $# -gt 0 ]]; do
    OPTIND=1
    while getopts "$OPTSTRING" opt; do
      case "$opt" in
        c) CODEC="$OPTARG" ;;  m) MODE="$OPTARG" ;;   p) MODE="voice" ;;
        v) VOL="$OPTARG" ;;    C) TONE="$OPTARG" ;;   G) GAP="$OPTARG" ;;
        L) LEAD="$OPTARG" ;;   E) TAIL="$OPTARG" ;;   g) GROUP="$OPTARG" ;;
        P) PORT="$OPTARG" ;;   t) DUR="$OPTARG" ;;    s) START="$OPTARG" ;;
        l) LOOP=1 ;;           T) OPUS_PT="$OPTARG" ;; o) OUT="$OPTARG" ;;
        n) DRY=1 ;;            *) usage ;;
      esac
      SEEN+=("-${opt}${OPTARG+ $OPTARG}")
      unset OPTARG
    done
    shift $((OPTIND - 1))
    [[ $# -gt 0 ]] && { POSITIONAL+=("$1"); shift; }
  done
}

# Check option values (everything but the audio file)
check_opts() {
  [[ -n "$VOL" ]] && ! isnum "$VOL" && die "Volume must be a number of dB, e.g. -6 or 3: $VOL"
  for pair in "L:$LEAD" "E:$TAIL" "G:$GAP"; do isnum "${pair#*:}" || die "-${pair%%:*} must be a number of ms: ${pair#*:}"; done
  [[ -n "$DUR" ]]   && ! isnum "$DUR"   && die "-t must be a number of seconds: $DUR"
  [[ -n "$START" ]] && ! isnum "$START" && die "-s must be a number of seconds: $START"
  [[ -n "$OUT" && -n "$LOOP" ]] && die "-o and -l can't be combined (the render would never end)"
  case "$MODE" in voice|music|none) ;; *) die "Unknown mode: $MODE (use voice, music, none)" ;; esac
  case "$CODEC" in g711u|g711a|g722|opus|opuswb) ;; *) die "Unknown codec: $CODEC (use g711u, g711a, g722, opus, opuswb)" ;; esac
}

# --- Presets: one file per preset, one option per line ("-c g722", "-p") ---
PRESET_DIR="${MCAST_PLAY_PRESETS:-${XDG_CONFIG_HOME:-$HOME/.config}/mcast/play-presets}"
# Call preset_name directly, not in $(...), so that die exits the script
preset_name() {
  [[ "$1" =~ ^[A-Za-z0-9_.][A-Za-z0-9_.-]*$ ]] || die "Invalid preset name: \"$1\" (use letters, digits, '-', '_', '.')"
}
preset_path() { echo "$PRESET_DIR/$1"; }
preset_need() { preset_name "$1"; [[ -f "$(preset_path "$1")" ]] || die "No preset named \"$1\" (see: $0 preset list)"; }
# Print a preset's options as arguments, one per line
preset_args() {
  local line
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line#"${line%%[![:space:]]*}"}"; line="${line%"${line##*[![:space:]]}"}"
    [[ -z "$line" || "$line" == \#* ]] && continue
    if [[ "$line" == *" "* ]]; then printf '%s\n' "${line%% *}" "${line#* }"; else printf '%s\n' "$line"; fi
  done < "$(preset_path "$1")"
}
# The setting an option line changes: -p and -m both set the mode
preset_key() { local k="${1:1:1}"; [[ "$k" == p ]] && k=m; echo "$k"; }
# Parse and check options, refusing files; leaves the normalised lines in SEEN
preset_check() {
  parse_opts "$@"
  [[ ${#POSITIONAL[@]} -eq 0 ]] || die "Presets hold options, not files: ${POSITIONAL[*]}"
  check_opts
}
preset_show() {
  local name="$1" args=()
  mapfile -t args < <(preset_args "$name")
  printf '%s: %s' "$name" "$(basename "$0")"
  [[ ${#args[@]} -gt 0 ]] && printf ' %q' "${args[@]}"
  echo
}
preset_cmd() {
  local cmd="${1:-list}"; shift || true
  local name="${1:-}" path line key new keep=() args=()
  [[ -n "$name" ]] && preset_name "$name"
  case "$cmd" in
    create|set)
      [[ -n "$name" ]] || die "Usage: $0 preset $cmd NAME options"; shift
      path=$(preset_path "$name")
      if [[ "$cmd" == create ]]; then
        [[ -e "$path" ]] && die "Preset \"$name\" already exists; change it with: $0 preset set $name ..."
      elif [[ $# -eq 0 ]]; then die "Usage: $0 preset set NAME options"
      fi
      [[ "$cmd" == set && ! -e "$path" ]] && echo "creating preset \"$name\""
      # Existing options first (checked too, in case of hand edits), then the new ones
      [[ -e "$path" ]] && mapfile -t args < <(preset_args "$name")
      ( preset_check "${args[@]}" ) || die "Preset \"$name\" has invalid options; fix or delete $path"
      preset_check "$@"; new=("${SEEN[@]}")
      if [[ ${#args[@]} -gt 0 ]]; then parse_opts "${args[@]}"; else SEEN=(); fi
      # A new option replaces the same setting (-p replaces -m and vice versa)
      for line in "${SEEN[@]}"; do
        key=$(preset_key "$line")
        for n in "${new[@]}"; do [[ $(preset_key "$n") == "$key" ]] && continue 2; done
        keep+=("$line")
      done
      mkdir -p "$PRESET_DIR" || die "Can't create $PRESET_DIR"
      printf '%s\n' "${keep[@]}" "${new[@]}" | grep -v '^$' > "$path.tmp" || true
      mv "$path.tmp" "$path"
      preset_show "$name" ;;
    unset)
      [[ -n "$name" && $# -ge 2 ]] || die "Usage: $0 preset unset NAME OPT... (e.g. C or -C)"; shift
      preset_need "$name"; path=$(preset_path "$name")
      mapfile -t args < <(preset_args "$name"); parse_opts "${args[@]}"
      for line in "${SEEN[@]}"; do
        key=$(preset_key "$line")
        for n in "$@"; do n="${n#-}"; [[ "$n" == p ]] && n=m
          [[ ${#n} -eq 1 && "$OPTSTRING" == *"$n"* && "$n" != : ]] || die "Not an option: $n"
          [[ "$n" == "$key" ]] && continue 2
        done
        keep+=("$line")
      done
      printf '%s\n' "${keep[@]}" | grep -v '^$' > "$path.tmp" || true
      mv "$path.tmp" "$path"
      preset_show "$name" ;;
    list|ls|show)
      if [[ -n "$name" ]]; then preset_need "$name"; preset_show "$name"; exit 0; fi
      shopt -s nullglob; local files=("$PRESET_DIR"/*)
      [[ ${#files[@]} -eq 0 ]] && echo "No presets in $PRESET_DIR; create one with: $0 preset create NAME options" >&2
      for path in "${files[@]}"; do [[ "$path" == *.tmp ]] || preset_show "$(basename "$path")"; done ;;
    copy|cp)
      [[ -n "$name" && -n "${2:-}" ]] || die "Usage: $0 preset copy FROM TO"
      preset_need "$name"; preset_name "$2"; path=$(preset_path "$2")
      [[ -e "$path" ]] && die "Preset \"$2\" already exists"
      cp "$(preset_path "$name")" "$path" && preset_show "$2" ;;
    delete|rm)
      [[ -n "$name" ]] || die "Usage: $0 preset delete NAME"
      preset_need "$name"; rm "$(preset_path "$name")" && echo "deleted $name" ;;
    *) die "Unknown preset command: $cmd (create, set, unset, list, copy, delete)" ;;
  esac
  exit 0
}
[[ "${1:-}" == preset ]] && { shift; preset_cmd "$@"; }

# @NAME arguments (where a file could go) expand to the preset's options. They go first, so
# options given on the command line win; then parse again.
parse_opts "$@"
PRESET_ARGS=(); REST=()
for p in "${POSITIONAL[@]}"; do
  if [[ "$p" == @* ]]; then
    preset_need "${p#@}"
    mapfile -t args < <(preset_args "${p#@}")
    PRESET_ARGS+=("${args[@]}")
  else REST+=("$p"); fi
done
if [[ ${#REST[@]} -ne ${#POSITIONAL[@]} ]]; then
  # The original arguments again, minus the @NAME ones (which only appear as positionals)
  ARGS=(); for a in "$@"; do [[ "$a" == @* && " ${POSITIONAL[*]} " == *" $a "* ]] || ARGS+=("$a"); done
  parse_opts "${PRESET_ARGS[@]}" "${ARGS[@]}"
fi
[[ ${#POSITIONAL[@]} -ne 1 ]] && { echo "Expected exactly one audio file, got: ${POSITIONAL[*]:-none}" >&2; usage; }
FILE="${POSITIONAL[0]}"
PAD=""; [[ "$FILE" == pad ]] && PAD=1
[[ -n "$PAD" || -f "$FILE" ]] || die "File not found: $FILE"
check_opts
if [[ -n "$PAD" ]]; then
  [[ -n "$START" ]] && die "-s doesn't apply to pad"
  # A loop repeats the whole prepared audio, so a chime, dead air or -t would recur every cycle
  [[ -n "$LOOP" && ( -n "$TONE" || -n "$DUR" || "$LEAD" != 0 ) ]] && die "pad -l can't be combined with -C, -t or -L"
  [[ -n "$LOOP" ]] && TAIL=0
fi

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

# Test pad: Cmaj7, Am7, Fmaj7, G6, 8 s each, 3 s cross-fades (notes two chords share are held),
# looping every 32 s, as in mcast's pad.rs. Each note is a pair of sines detuned 5/32 Hz either
# side with its own slow swell; bass notes add a little 2nd and 3rd harmonic for small speakers.
# Every frequency is a multiple of 1/32 Hz, so 32 s is exactly one cycle and -l loops without a seam.
pad_expr() {
  awk '
  # 0 -> 1 as x goes 0 -> 1, flat at both ends
  function E(x) { return "(0.5-0.5*cos(PI*clip(" x ",0,1)))" }
  BEGIN {
    split("48 55 64 67 71|45 52 64 67 72|41 60 64 69 72|43 62 64 67 71", ch, "|")
    for (c = 1; c <= 4; c++) { n = split(ch[c], v, " "); for (j = 1; j <= n; j++) {
      in_ch[v[j], c] = 1; used[v[j]] = 1; if (j == 1) bass[v[j]] = 1 } }
    PI = atan2(0, -1); i = 0
    for (m = 0; m < 128; m++) if (m in used) {
      f = int(440 * 2 ^ ((m - 69) / 12) * 32 + 0.5) / 32
      w = (m in bass) ? 0.8 : 1 - (m - 60) * 0.015
      lv = (m in bass) ? 1.5 : 1; wt[m] = w * lv
      # Gain: the sum of the windows of the chords holding this note
      g = ""
      for (c = 1; c <= 4; c++) if (in_ch[m, c]) {
        d = sprintf("(mod(t+%g,32)-4)", 1.5 - 8 * (c - 1) + 4 + 32)
        g = g (g == "" ? "" : "+") E("(" d "+1.5)/3") "*" E("(9.5-" d ")/3")
      }
      sw = int((0.05 + 0.07 * ((i * 7) % 11) / 10) * 32 + 0.5) / 32
      tone = ""
      for (h = 1; h <= ((m in bass) ? 3 : 1); h++) {
        hl = (h == 1) ? 1 : (h == 2) ? 0.35 : 0.15
        tone = tone (tone == "" ? "" : "+") sprintf("%g*(sin(%.10g*t+%g)+sin(%.10g*t+%g))", hl * 0.5,
          2 * PI * h * (f - 5 / 32), i * 1.3, 2 * PI * h * (f + 5 / 32), i * 2.1 + 0.7)
      }
      terms = terms (terms == "" ? "" : "+") sprintf("(%s)*%g*(1+0.25*sin(%.10g*t+%g))*(%s)", g, w, 2 * PI * sw, i * 0.9, tone)
      i++
    }
    # Worst case: a whole chord at the top of its swell, all in phase; peak at -12 dBFS
    for (c = 1; c <= 4; c++) { s = 0; for (m in used) if (in_ch[m, c]) s += wt[m] * 1.25; if (s > pk) pk = s }
    print sprintf("%.6g*(%s)", 10 ^ (-12 / 20) / pk, terms)
  }'
}

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
if [[ -n "$PAD" ]]; then
  # One cycle to loop (faded in once, when sending starts), or a finished piece with 2 s fades
  if [[ -n "$LOOP" ]]; then PAD_DUR=34   # trimmed to 1-33 s below
  else
    PAD_DUR="${DUR:-64}"
    FX+=("afade=t=in:d=2" "afade=t=out:st=$(awk "BEGIN{print ($PAD_DUR>4)?$PAD_DUR-2:$PAD_DUR/2}"):d=2")
  fi
else
  [[ -n "$START" ]] && FX+=("afade=t=in:d=0.05")
  [[ -n "$DUR" ]]   && FX+=("afade=t=out:st=$(awk "BEGIN{print ($DUR>0.3)?$DUR-0.3:0}"):d=0.3")
fi
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
# The cycle to loop comes from the middle, clear of the resampler's ringing at the edges
[[ -n "$PAD" && -n "$LOOP" ]] && FX+=("atrim=start=1:end=33" "asetpts=PTS-STARTPTS")
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

if [[ -n "$PAD" ]]; then
  SRC_IN=(-f lavfi -i "aevalsrc='$(pad_expr)':s=16000:d=${PAD_DUR}")  # nothing in it above 2 kHz
else
  IN=()
  [[ -n "$START" ]] && IN+=(-ss "$START")
  [[ -n "$DUR" ]]   && IN+=(-t "$DUR")
  SRC_IN=("${IN[@]}" -i "$FILE")
fi
BASE=(ffmpeg -hide_banner -loglevel error)

if [[ -n "$TONE" ]]; then
  # Tone: mono, band-limited for the codec, level-matched near the announcement, then the gap.
  # Not voice-processed: denoise and de-essing would smear a bell.
  TFX="${MONO},highpass=f=200${LP:+,lowpass=f=${LP}},loudnorm=I=-18:TP=-3,${NORM},apad=pad_dur=${GAP}ms"
  if [[ -n "$TONE_FILE" ]]; then TIN=(-i "$TONE_FILE"); else TIN=("${CHIME_SRC[@]}"); fi
  GRAPH="[0:a]${TFX}[tone];[1:a]${AF_MAIN}[main];[tone][main]concat=n=2:v=0:a=1,${AF_POST}[out]"
  PREP_IN=("${TIN[@]}" "${SRC_IN[@]}")
else
  GRAPH="[0:a]${AF_MAIN},${AF_POST}[out]"
  PREP_IN=("${SRC_IN[@]}")
fi

DESC="v${VERSION} | $([[ -n "$PAD" ]] && echo "pad" || basename "$FILE") | ${CODEC} | ${MODE}${VOL:+ | ${VOL} dB}${TONE_DESC:+ | ${TONE_DESC}}"
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
  # A looped pad fades in here, once, rather than at the start of every cycle
  [[ -n "$PAD" && -n "$LOOP" ]] && AF2="afade=t=in:d=2,$AF2"
  CMD=("${BASE[@]}" -stats "${LOOPIN[@]}" -i "$TMPWAV" -af "$AF2,arealtime" "${ENC[@]}" -f rtp "$URL")
  echo ">>> $DESC | ${GROUP}:${PORT}${LOOP:+ | looping, q to stop}"
  if [[ -n "$DRY" ]]; then
    printf '%q ' "${PREP[@]}"; echo; printf '%q ' "${CMD[@]}"; echo
  else
    "${PREP[@]}" || exit $?
    "${CMD[@]}"
  fi
fi
