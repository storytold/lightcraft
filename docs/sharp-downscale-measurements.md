# Sharp downscale threshold measurement

Compare the Sharp export resizer with prefilter thresholds k > 1.0 and k > 1.25, where k is source width / output width. Smooth previews retain their existing threshold. These numbers measure the resize operation; they do not establish perceptual quality, halo visibility, or behavior under every geometric warp.

Six existing CC0-1.0 RAW corpus records were verified against their manifest SHA-256 before decoding through the engine. The native sensor decode was required (not the embedded JPEG). Numeric linear Rec.2020 luminance uses weights 0.2627, 0.6780, 0.0593.

## Method

- Five fixed 500 x 500 source tiles per file, at 20%/80% corners and center of the available tile-origin range. No content-based tile selection.
- Export scales 0.9, 0.8, 0.7; actual Frame::sample with Lanczos3. The old path at k <= 1.25 is reproduced with its bilinear mapping; at 0.7 both thresholds use the same Lanczos path, providing an unchanged control.
- Alias proxy: mirror-pad each source tile by 250 pixels; FFT-isolate frequencies outside the target Nyquist square (either axis greater than scale/2 cycles/source pixel), inverse transform, crop to the original tile, then resize that signed high-band component through each path. Measure mean-square output energy, excluding 12 output pixels on each border. This measures folded energy from the removed source band rather than calling all output high frequencies aliases.
- Edge acutance proxy: mean of the strongest 5% Sobel gradient magnitudes in the resized ordinary tile, divided by its 95th-minus-5th-percentile luminance contrast; same border exclusion. Average five tiles per file. Percent changes compare threshold 1.0 against 1.25.

## Results across six files

- Scale 0.9: alias energy -25.48% (range -26.39% to -20.30%); edge acutance +4.62% (range +3.15% to +8.37%).
- Scale 0.8: alias energy -65.59% (range -66.07% to -62.74%); edge acutance +3.59% (range +2.39% to +6.03%).
- Scale 0.7: alias energy +0.00% (range +0.00% to +0.00%); edge acutance +0.00% (range +0.00% to +0.00%).

## Per-file changes

- Canon EOS 7D, 0.9: alias energy -25.70%; edge acutance +8.37%.
- Canon EOS 7D, 0.8: alias energy -64.35%; edge acutance +6.03%.
- Canon EOS 7D, 0.7: alias energy +0.00%; edge acutance +0.00%.
- Sony ILCE-7M3, 0.9: alias energy -24.27%; edge acutance +6.70%.
- Sony ILCE-7M3, 0.8: alias energy -62.74%; edge acutance +5.93%.
- Sony ILCE-7M3, 0.7: alias energy +0.00%; edge acutance +0.00%.
- Nikon D7000, 0.9: alias energy -26.02%; edge acutance +3.63%.
- Nikon D7000, 0.8: alias energy -65.91%; edge acutance +3.17%.
- Nikon D7000, 0.7: alias energy +0.00%; edge acutance +0.00%.
- Panasonic DMC-GF6, 0.9: alias energy -25.25%; edge acutance +4.42%.
- Panasonic DMC-GF6, 0.8: alias energy -65.70%; edge acutance +3.94%.
- Panasonic DMC-GF6, 0.7: alias energy +0.00%; edge acutance +0.00%.
- Pentax K-5, 0.9: alias energy -26.39%; edge acutance +3.15%.
- Pentax K-5, 0.8: alias energy -66.07%; edge acutance +2.39%.
- Pentax K-5, 0.7: alias energy +0.00%; edge acutance +0.00%.
- Fujifilm X-T3, 0.9: alias energy -20.30%; edge acutance +4.83%.
- Fujifilm X-T3, 0.8: alias energy -65.48%; edge acutance +3.23%.
- Fujifilm X-T3, 0.7: alias energy +0.00%; edge acutance +0.00%.

## Source records

- [Canon EOS 7D, record 131](https://raw.pixls.us/getfile.php/131/nice/Canon%20-%20EOS%207D%20-%20RAW%20%283:2%29.CR2), CC0-1.0; SHA-256 `b5e47c5fcf7332ac03e0134926f17a338a42e68c1fd7f83e16f45f4b767544e8`.
- [Sony ILCE-7M3, record 2414](https://raw.pixls.us/getfile.php/2414/nice/Sony%20-%20ILCE-7M3%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW), CC0-1.0; SHA-256 `250784580ea527442c09004417bb0eead484f2bf3ee8f9121a776ac65bb50d0f`.
- [Nikon D7000, record 958](https://raw.pixls.us/getfile.php/958/nice/Nikon%20-%20D7000%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF), CC0-1.0; SHA-256 `639945753240005d80488c0462c01e73a7ab82c96d3d1cdb6661f02e1b3c1319`.
- [Panasonic DMC-GF6, record 739](https://raw.pixls.us/getfile.php/739/nice/Panasonic%20-%20DMC-GF6%20-%204:3.RW2), CC0-1.0; SHA-256 `dace6b3818f00a5d4f7cc1cfc29744d15dcf7dd8684c882bea4a81c6ef670ad0`.
- [Pentax K-5, record 1479](https://raw.pixls.us/getfile.php/1479/nice/Pentax%20-%20K-5%20-%2014bit%20%283:2%29.DNG), CC0-1.0; SHA-256 `6f6419209340d07d597ccca66dc367d64cff3308ef2694c8f90c6259f3a5c431`.
- [Fujifilm X-T3, record 2783](https://raw.pixls.us/getfile.php/2783/nice/Fujifilm%20-%20X-T3%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF), CC0-1.0; SHA-256 `95b33021160b239ceb1a09d46a1b29cb60cb7dd47581feb126b337c219091754`.
