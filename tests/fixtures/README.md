# Test fixtures

`stereo_sine_44k1.flac` — 0.2 s, 44.1 kHz, stereo, 16-bit FLAC (13 KB).
Left channel: 440 Hz sine, right channel: 880 Hz sine, both peaking at 0.5.
Generated once with ffmpeg (not needed at test time):

```
ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.2" \
       -f lavfi -i "sine=frequency=880:sample_rate=44100:duration=0.2" \
       -filter_complex "[0:a][1:a]join=inputs=2:channel_layout=stereo,volume=4[a]" \
       -map "[a]" -sample_fmt s16 -c:a flac -compression_level 8 -bitexact stereo_sine_44k1.flac
```

WAV fixtures are written at test time with `superb_io::wav::write`.
