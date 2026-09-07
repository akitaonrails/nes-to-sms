# Gameplay recordings

OBS captures supplied by the project owner on September 7, 2026. The videos
show the translated SMS builds in an emulator, not native-hardware speed
measurements. No gameplay was sped up, cropped or deliberately trimmed;
audio and the original 30 fps cadence are retained. Original OBS files remain
outside the repository and were not modified.

| Recording | Duration | Original size | Web copy |
|---|---:|---:|---:|
| [Super Mario Bros.](smb-gameplay.mp4?raw=true) | 1:31 | 69,986,304 bytes | 5,453,425 bytes |
| [Castlevania](castlevania-gameplay.mp4?raw=true) | 2:52 | 132,220,600 bytes | 16,384,444 bytes |

Combined size falls from 202.2 MB to 21.8 MB (89.2% smaller). Both copies use
1280×720 H.264 video, YUV 4:2:0, AAC stereo audio at 96 kb/s, and an MP4
header placed before the media payload for progressive playback.

The SMB source declares 2,723 frames but decodes to 2,722; the web copy
contains all 2,722 decoded frames. This explains its one-frame duration
difference (90.733 seconds versus 90.767 in the source container).

## Re-encode

Use an existing FFmpeg installation and substitute the source/output paths:

```sh
ffmpeg -n -i recording.mp4 \
  -map 0:v:0 -map 0:a:0 -map_metadata -1 -map_chapters -1 \
  -vf 'scale=1280:720:flags=lanczos,setsar=1' \
  -c:v libx264 -preset slow -crf 22 -threads 4 -pix_fmt yuv420p \
  -c:a aac -b:a 96k -movflags +faststart docs/media/gameplay.mp4
```

Previews are unaltered frames extracted from the encoded copies: SMB at
15 seconds and Castlevania at 105 seconds. For example:

```sh
ffmpeg -n -ss 15 -i docs/media/smb-gameplay.mp4 \
  -frames:v 1 -update 1 docs/media/smb-gameplay.png
```

## Git LFS

The root `.gitattributes` tracks `docs/media/*.mp4` with Git LFS. PNG previews
stay in ordinary Git so the README does not need video downloads to show them.
Use [GitHub's LFS setup instructions](https://docs.github.com/en/repositories/working-with-files/managing-large-files/configuring-git-large-file-storage)
when setting up a new contributor checkout:

```sh
git lfs install --local
git lfs pull
git lfs ls-files
```

Before committing a replacement, verify it fully decodes with
`ffmpeg -v error -xerror -i docs/media/gameplay.mp4 -f null -`, inspect its
picture/audio, and confirm `git show :docs/media/gameplay.mp4` contains an LFS
pointer after staging. Do not add original OBS recordings or ROM files.

Game artwork and music remain the property of their respective rights holders;
the project code license does not relicense them.
