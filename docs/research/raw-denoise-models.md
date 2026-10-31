# RAW-Denoising-Modelle für LightCraft

Stand: 2026-10-28. Recherche und technische Vorprüfung; keine Produktfreigabe.
Untersucht: Upstream `storytold/lightcraft@59a5d00c608dd4dc4b3c0d9cd4e914e81aa86ce5`,
Fork `schattie93/lightcraft@3a3e49af53aeff55248419234a05d1a262094092`.
Der Forschungsbranch beginnt am genannten Upstream-Commit; der Fork war 67 Commits zurück, ohne eigene abweichende Commits.

## Entscheidung

**RawNIND UtNet2 Bayer zuerst weiterverfolgen und die bestehende Integration nutzen.**
Es verarbeitet echte Bayer-RAW-Daten vor dem Demosaicing, übernimmt das Demosaicing
und besitzt bereits ein veröffentlichtes ONNX-Paket mit überprüfbaren Release-Metadaten.
Ein neuer Denoiser, Installer oder ONNX-Runtime ist dafür nicht erforderlich.

Die wichtigste neue Erkenntnis betrifft die Lizenz: Das aktuelle Autoren-README
nennt die Gewichte ausdrücklich als dual lizenziert unter **GPL3 + CC-BY-4.0**.
LightCraft und das konkrete darktable-ai-ONNX-Paket deklarieren weiterhin ausschließlich
GPL-3.0. Das ist eine aussichtsreiche permissive Lizenzspur, aber noch keine
hashgebundene Bestätigung, dass genau das verwendete ONNX-Artefakt unter CC BY 4.0
weitergegeben werden darf. Die Formulierung mit „+“ sollte außerdem eindeutig als
Wahlrecht bestätigt werden. Keine Lizenzmetadaten oder Download-Defaults ändern,
bevor diese Zuordnung dokumentiert ist. [R1–R4]

**Kein Kandidat erhält in dieser Untersuchung eine uneingeschränkte Auslieferungsfreigabe.**
Kommerzielle Verwendung und Weitergabe sind unterschiedliche Fragen.
GPL verbietet kommerzielle Verwendung nicht; separat heruntergeladene Gewichte
lösen die Fragen zu Kombination, Weitergabe und Corresponding Source aber nicht automatisch.
Bei mehreren permissiven Codeprojekten fehlt eine ausdrückliche Lizenz der externen Gewichte.
Ein restriktiver Datensatz macht Gewichte nicht automatisch ebenso restriktiv; umgekehrt
ersetzt die Code-Lizenz keine Prüfung der Gewichte und Datenherkunft.

DeepPRIME bleibt ein Qualitätsziel. Es gab hier keinen direkten Vergleich und keine
Messung einer Gleichwertigkeit. SIDD-sRGB-Ergebnisse, RAW-PSNR und subjektiv angenehme
Ausgaben sind keine austauschbaren Qualitätsnachweise.

## Methode und Nachweisstufen

Primärquellen: Autoren-Repositories, LICENSE-Dateien, Modellkarten, Release-API,
Originalpapers und Datensatzseiten. Suchtreffer und Modellspiegel dienten nur zum Finden
der Originalquellen. GPL-Implementierungen wurden nicht als Portierungsvorlage verwendet;
untersucht wurden dort Lizenztexte, Dokumentation und Modellmetadaten.

Downloadstatus wird ausdrücklich unterschieden:

- **A:** Binärdatei als GitHub-Release-Asset oder Repository-Blob nachgewiesen.
  Das bestätigt die veröffentlichte Datei, nicht deren erfolgreichen lokalen Download.
- **B:** Originalprojekt verlinkt Gewichte; tatsächlicher Binärabruf hier nicht verifiziert.
- **N/A:** kein trainiertes Modell nötig.
- SHA-256 wird nur als selbst berechnet, Publisher-Metadatum oder bereits vorhandener
  LightCraft-Pin bezeichnet. Git-Blob-SHAs sind keine Datei-SHA-256.

Es wurden keine Gewichte heruntergeladen, ausgeführt, committed oder weiterverteilt.
Es wurde kein automatischer Download aktiviert. Keine bestehende PR wurde verändert.
Die rechtlichen Einstufungen sind eine dokumentierte technische Lizenzprüfung,
keine verbindliche rechtliche Freigabe.

## Vorhandene LightCraft-Pipeline

Maßgeblich sind [docs/denoise.md](../denoise.md),
[denoise-core](../../crates/denoise-core/README.md),
[denoise](../../crates/denoise/README.md),
[Modellkatalog](../../crates/denoise-core/src/known.rs) und
[ONNX-Parser](../../crates/denoise/src/onnx.rs).

- Kein ONNX Runtime im Produkt: eigener begrenzter ONNX-Parser, pure-Rust-CPU-Ausführung
  mit faer/GEMM und GPU-Zwilling über wgpu/WGSL. Ein erfolgreicher Export für ONNX Runtime
  bedeutet daher noch keine LightCraft-Kompatibilität.
- Vertrag `bayerToRgb`: schwarzpegelkorrigierte, normalisierte, nicht weißabgeglichene
  RGGB-Ebenen `[1,4,T,T]` → Kamera-RGB `[1,3,2T,2T]`.
  RawNIND: T=512, Überlappung=64, Gain-Matching mit nominell 1e6.
- Unterstützte Operatoren: Conv 1×1/3×3, stride=1, ohne Gruppen/Dilation,
  anschließendes LeakyRelu; ConvTranspose 2×2/stride=2; MaxPool 2×2/stride=2;
  zweifaches Kanal-Concat vor Conv; abschließendes DepthToSpace block=2, CRD.
  Allgemeines Add, Mul, Softmax, LayerNorm, MatMul, Gather, Unfold oder Depthwise-Conv
  gehören nicht zum dokumentierten Vertrag.
- Bayer-Packing, Kachelung, Blend/Highlight-Schutz, Manifest-/Hash-/Lizenzprüfung,
  Installations-Selbsttest, CPU/GPU-Vergleich, Worker-Budget, Fehlerbehandlung,
  `.lcdn`-Cache und Preview-/Exportintegration existieren bereits.
- Standardkatalog leer; `rawnind-model` bleibt eine gesonderte Release-Policy-Entscheidung.
  X-Trans/Foveon und RGB-Bilder sind vom aktuellen Modellvertrag nicht abgedeckt.
- Repo-Regeln: Produkt pure Rust, keine C/C++-Abhängigkeiten und keine Übernahme von
  GPL/LGPL-Code. Ein externer Python-/C-Versuch wäre ausschließlich Forschung.

**Doppelte Implementierungen vermeiden:** RawNIND nicht erneut exportieren oder integrieren,
bevor der vorhandene ONNX-Pin getestet ist. OpenDenoise nicht als zweite Import-/DNG-/
Kachel-Pipeline übernehmen. Bei Bayer→Bayer-Modellen zunächst den zusätzlichen Domain-Vertrag
entwerfen und danach die vorhandene Verarbeitung wiederverwenden.

## Lizenzmatrix

„Ungeklärt“ bedeutet: keine ausreichende ausdrückliche, artefaktspezifische Erlaubnis in
den hier geprüften Quellen. Es bedeutet nicht, dass die Nutzung definitiv verboten ist.

| Projekt | Quellcode | Gewichte separat | Trainingsdaten / Herkunft | Kommerziell / Weitergabe | Namensnennung und offene Punkte |
|---|---|---|---|---|---|
| **RawNIND** | GPL-3.0; kein Codeport ins permissive Rust-Produkt | Autoren: GPL3 + CC-BY-4.0; konkretes ONNX-Paket: GPL-3.0 | RawNIND reale noisy/clean RAW-Paare, laut Modellkarte CC BY 4.0 / CC0 je Bild | Kommerzielle Nutzung unter beiden genannten Lizenzen grundsätzlich möglich. CC-BY-Weitergabe erst nach Klärung von Wahlrecht und exaktem Checkpoint/Export; GPL-Weitergabe mit deren Pflichten | Brummer / De Vleeschouwer, Modell-/Paper-/Lizenzlink, Änderungen beim Export nennen. GPL-Pfad: Lizenz, Hinweise und anwendbare Source-Pflichten. Per-image-Datennachweise bei eigenem Training sichern; Dataverse hier nicht direkt abrufbar. [R1–R4] |
| **OpenDenoise / SCUNet** | OpenDenoise MIT; SCUNet Apache-2.0 | SCUNet-Release-Gewichte nicht gesondert eindeutig lizenziert in geprüfter Dokumentation; Apache-Repolizenz ist Indiz, keine separate Bestätigung | SCUNet synthetische Degradation sauberer RGB-Bilder; Paper nennt WED, DIV2K, Flickr2K. Keine pauschale Freigabe aller Ausgangsbilder nachgewiesen | Code kommerziell verwendbar/weitergebbar mit MIT-/Apache-Pflichten; Gewichte und Datennachweise **ungeklärt** | MIT-Copyright; Kai Zhang / SCUNet, Apache-Lizenz und vorhandene Notices, Änderungen. OpenDenoise MIT lizenziert nicht automatisch SCUNet-Gewichte oder rawpy/LibRaw. [R5–R7] |
| **PMRID** | Apache-2.0 | Zwei Checkpoints direkt im Apache-Repo, aber keine ausdrückliche separate Gewichte-Erklärung gefunden | Laut Paper 214 ausgewählte Sony-a7S-II-Langzeit-RAWs aus SID; synthetisches Poisson-Gaussian-Rauschen und Reno10x-Sensorkalibrierung. PMRID/Reno10x-Daten sind auch Benchmark-/Kalibrierdaten, nicht mit Trainingsbildern verwechseln | Code ja; Anwendbarkeit der Repo-Lizenz auf Checkpoints und konkrete Datenbedingungen vor Distribution bestätigen | MegEngine/Megvii-/Autorenhinweise und Apache-Lizenz erhalten; Änderungen markieren. SID-Bedingungen und Gewichtebestätigung archivieren. [R8,R9,R16] |
| **Restormer** | MIT; BasicSR-/Fremdkomponenten gesondert beachten | Externe Drive-Checkpoints: separate Gewichte-Lizenz nicht ausdrücklich gefunden | Real-Denoising: SIDD; Gaussian: DIV2K/Flickr2K/WED/BSD. SIDD-Seite nennt MIT, weitere Datensätze nicht pauschal geklärt | Code ja; externe Gewichte **ungeklärt** | Zamir und Mitwirkende/MIT-Text; BasicSR-Notices soweit übernommen; Paperzitat erbeten. Keine RAW-Freigabe aus SIDD-RGB ableiten. [R10,R15] |
| **KBNet** | MIT | Externe Baidu/OneDrive-Dateien: separate Gewichte-Lizenz **ungeklärt** | SIDD/SenseNoise und Gaussian-Varianten; veröffentlichte sidd.yml enthält sogar Gaussian-Dataset-/S3-Platzhalter. Exakte Trainingsprovenienz je Checkpoint daher zusätzlich offen | Code ja; Gewichte-/Datenweitergabe nicht freigegeben | Zhang Yi/MIT-Text, Notices der übernommenen BasicSR-/NAFNet-/Restormer-Anteile; Paperzitat erbeten. [R11] |
| **Uformer** | MIT | Externer SharePoint-Checkpoint: separate Gewichte-Lizenz **ungeklärt** | SIDD-Medium **sRGB**, laut SIDD-Seite MIT | Code ja; Gewichte-Weitergabe erst nach Bestätigung | Zhendong Wang/MIT-Text; Fremdhinweise erhalten, Paperzitat erbeten. [R12,R15] |
| **NAFNet** | MIT plus BasicSR-Apache-Hinweise | Externe Drive/Baidu-Checkpoints: ausdrückliche separate Lizenz nicht gefunden | SIDD-sRGB für Denoising; SIDD MIT | Code ja; Gewichte **ungeklärt**; kein fertiger nativer Bayer-Ersatz | megvii-model/MIT und relevante BasicSR-Hinweise; Paperzitat erbeten. [R13,R15] |
| **ELD** | MIT | Externes Modellpaket ohne hier bestätigte Gewichte-Lizenz | SID und synthetisches Kamerarauschen; ELD-Aufnahmen dienen laut README Evaluation. Älterer Business-License-Hinweis steht neben späterer Trainingscode-Freigabe | Code ja; Gewichte, Datenbedingungen und historischer Hinweis **ungeklärt** | Kaixuan Wei/MIT; Paperzitat erbeten. Nicht automatisch die gesamte Methode als proprietär oder freigegeben behandeln. [R14] |
| **SID / Learning to See in the Dark** | README nennt MIT | Sony-/Fuji-TensorFlow-Modelle; keine separate explizite Gewichte-Erklärung gefunden | Eigene Sony a7S II / Fuji X-T2 RAW-Paare; README-Lizenz MIT, artefaktbezogene Zuordnung archivieren | Grundsätzlich permissive Projektdeklaration; Gewichte-/Datenumfang vor Weitergabe bestätigen | Chen Chen et al., MIT-Hinweise; Paperzitat für Code/Daten erbeten. [R16] |
| **LED / UltraLED** | README ausdrücklich CC BY-NC 4.0, kommerzielle Erlaubnis separat erforderlich | Kein unabhängiger kommerzieller Grant für die angebotenen Modelle belegt | Virtuelle Kameras/ELD-Noise, SID-/kameraspezifische Anpassung; genaue Daten je Modellkonfiguration prüfen | Für kommerzielles LightCraft **ausgeschlossen ohne zusätzliche Lizenz**, auch RAW-Restormer-/NAFNet-Varianten nicht mit deren MIT-Code verwechseln | Autoren-/Lizenz-/Änderungshinweise; kommerzielle Lizenzstelle im README. Kein Kontakt im Rahmen dieser Arbeit aufgenommen. [R17] |
| **YOND** | Kernmodule laut Autoren mit PyArmor verschleiert; kein vollständig offener, frei portierbarer Kern nachgewiesen | SNR-Net-Checkpoints verlinkt, Gewichte-Lizenz **ungeklärt** | DIV2K und SID-Sony-Ground-Truth, Unprocessing zu pseudo-RAW | Nicht für Produktintegration freigegeben; Quellen-/Lizenz- und Portierungsblocker | Autoren/Paper nennen; keine Bilder ins öffentliche Online-Demo hochgeladen. [R18] |
| **GALOSH** | Apache-2.0; NOTICE vorhanden; Paper/Figuren separat CC BY 4.0 | Keine Gewichte | Kein Training. SIDD/RawNIND nur Evaluation | Code grundsätzlich kommerziell/weitergebbar unter Apache; Patentumfang und Rechteinhaberschaft vor Produktport prüfen | luxgrain, LICENSE, NOTICE, Änderungsvermerke erhalten. README nennt US-Provisional 64/058,343 vom 2026-05-06. Apache §3 enthält einen begrenzten Contributor-Patentgrant; Patentmeldung ist weder automatisches Verbot noch umfassende FTO-Freigabe. [R19] |

Die notwendigen Lizenztexte/Notices sind von freiwilligen wissenschaftlichen Zitierbitten
zu unterscheiden. CC BY 4.0 verlangt angemessene Zuschreibung, Lizenzlink und Kennzeichnung
von Änderungen; MIT verlangt Erhalt von Copyright-/Lizenztext, Apache zusätzlich relevante
NOTICE-/Änderungshinweise. Bei Modellen mit ungeklärter Lizenz ist Zuschreibung allein keine Erlaubnis.

## Gewichte: Originalquellen, Versionen, Formate, Abrufnachweise

| Kandidat / Modellversion | Originalquelle / konkrete Datei | Format und Status | Integrität |
|---|---|---|---|
| RawNIND UtNet2 Bayer 1.0 | [darktable-ai release-5.6.0](https://github.com/darktable-org/darktable-ai/releases/tag/release-5.6.0), [rawdenoise-nind.dtmodel](https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/rawdenoise-nind.dtmodel) | **A:** ZIP-Paket, 57.700.134 B; enthält laut Modellkarte model_bayer.onnx und model_linear.onnx; opset 20, FP32, statisch 512 | Publisher-API digest SHA-256: `d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af`. Gegen Katalog geprüft, nicht lokal nachgerechnet |
| RawNIND model_bayer.onnx | Gleiches Paket; [Originalcheckpoint](https://drive.google.com/file/d/1dFTLeljWi9wwojcZUsam8bE31JdYy3oM/view) laut model.yaml | ONNX laut LightCraft 31.056.425 B. Checkpoint: DenoiserTrainingBayerToProfiledRGB_4ch_2024-02-21-bayer_ms-ssim_mgout_notrans_valeither_-4, Iteration 4.350.000, .pt | LightCraft-Pin: `da27509dab6a2915da67e988acd86cf71f9d5bbc8d1aa0ed32933578a887b901`; **nicht** neu berechnet. Drive hier nicht abrufbar |
| RawNIND weitere Originalgewichte | [Autoren-Drive](https://drive.google.com/drive/folders/12Uc5sT4OWx02sviUS_boDk1zDSDRVorw), [Modellkonfiguration](https://github.com/darktable-org/darktable-ai/blob/master/models/rawdenoise-nind/model.yaml) | **B:** .pt; Linear-Checkpoint 2024-10-09-prgb…-1, Iteration 3.900.000; andere/preupsample Varianten nicht mit obigem Pin verwechseln | Nicht ermittelt |
| SCUNet real PSNR / GAN | [KAIR v1.0](https://github.com/cszn/KAIR/releases/tag/v1.0), [PSNR](https://github.com/cszn/KAIR/releases/download/v1.0/scunet_color_real_psnr.pth), [GAN](https://github.com/cszn/KAIR/releases/download/v1.0/scunet_color_real_gan.pth) | **A:** Release-API bestätigt .pth, PSNR 71.982.841 B, GAN 71.982.835 B. SCUNet-Downloadskript verweist genau auf KAIR | API digest jeweils null; kein lokaler SHA-256 |
| PMRID ECCV2020 | [models bei 8ebb9e8](https://github.com/MegEngine/PMRID/tree/8ebb9e8e96559881dee957f34243933c5beb77dd/models) | **A:** torch_pretrained.ckp 4.183.200 B; mge_pretrained.ckp 4.138.226 B, echte Repository-Blobs | Git-Blob-SHAs 85772aa732674ea8aa06dcf9f7f553e95e2059ef / cb3892af7153d1e0cb10698f291fbb1237bbd845; **keine SHA-256**, kein lokaler Abruf |
| Restormer Real Denoising | [Autoren-Drive](https://drive.google.com/file/d/1FF_4NTboTWQ7sHCq4xhyLZsSl0U0JfjH/view), verlinkt in Denoising/README.md | **B:** PyTorch .pth; real_denoising; Gaussian-Modelle in [separatem Ordner](https://drive.google.com/drive/folders/1Qwsjyny54RZWa7zC4Apg7exixLBo4uF0). Drive-Abruf hier fehlgeschlagen | Nicht ermittelt |
| KBNet-S / SIDD | [Autoren-Baidu](https://pan.baidu.com/s/1JhjJwhMwrfW7aFW_dKYioQ?pwd=nqcn), OneDrive-Alternative im [README](https://github.com/zhangyi-3/KBNet) | **B:** sidd.pth laut Denoising/Options/sidd.yml; Baidu hier nicht abrufbar; Varianten Gaussian/SenseNoise nicht vertauschen | Nicht ermittelt |
| Uformer-B / SIDD | [Autoren-SharePoint](https://mailustceducn-my.sharepoint.com/:u:/g/personal/zhendongwang_mail_ustc_edu_cn/Ea7hMP82A0xFlOKPlQnBJy0B9gVP-1MJL75mR4QKBMGc2w?e=iOz0zz) | **B:** PyTorch-Checkpoint; finale heruntergeladene Datei/Verpackung nicht verifiziert, SharePoint hier nicht abrufbar | Nicht ermittelt |
| NAFNet-SIDD-width32 / width64 | [32](https://drive.google.com/file/d/1lsByk21Xw-6aW7epCwOQxvm6HYCQZPHZ/view), [64](https://drive.google.com/file/d/14Fht1QQJ2gMlk4N1ERCRuElg8JfjrWWR/view), Original-README | **B:** PyTorch .pth; keine erfolgreiche Binärübertragung hier belegt | Nicht ermittelt |
| ELD Sony-/SID-Varianten | [Autoren-Drive](https://drive.google.com/drive/folders/1QoEhB1P-hNzAc4cRb7RdzyEKktexPVgy), Original-README | **B:** PyTorch-Checkpoints im Paket; Version/exakte Dateien nicht verifiziert | Nicht ermittelt |
| SID Sony / Fuji | [download_models.py](https://github.com/cchen156/Learning-to-See-in-the-Dark/blob/master/download_models.py) mit Original-Drive-IDs | **B:** TensorFlow-Checkpoint-Dateien model.ckpt.data-00000-of-00001 und .meta; Projekt nennt ca. 84 MB je Modell; vollständiger Satz laut FAQ erforderlich | Nicht ermittelt |
| LED / UltraLED | [Original-Model-Zoo](https://github.com/Srameo/LED/blob/main/docs/pretrained-models.md), [Drive](https://drive.google.com/drive/folders/11MYkjzbPIZ7mJbu9vrgaVC-OwGcOFKsM) | **B:** PyTorch-Checkpoints, modellabhängige Konfiguration; keine freie kommerzielle Freigabe | Nicht ermittelt |
| YOND / SNR-Net | [Autoren-README/Baidu](https://github.com/fenghansen/YOND_public) | **B:** checkpoints verlinkt, Format/Dateiname nicht durch Binärabruf verifiziert | Nicht ermittelt |
| GALOSH | [Originalrepository](https://github.com/luxgrain/GALOSH), [Releases](https://github.com/luxgrain/GALOSH/releases) | **N/A:** C-Referenz, OpenCL/Vulkan; keine Modellgewichte | Quellstand 11de0593cc8091933ad76d1d6d873f7dee537f15; Binary-Hashes nicht ermittelt |

**Abrufbarkeit nicht überbehaupten:** Für RawNIND, SCUNet und PMRID ist die Existenz
konkreter Binärartefakte über die GitHub-API bestätigt. Für die anderen Projekte ist nur
die Veröffentlichung der Originalverknüpfung belegt. Fehlgeschlagene Drive-/SharePoint-/
Baidu-Abfragen beweisen nicht, dass ein Link für andere Nutzer tot ist. Ohne erfolgreiche
Übertragung gibt es keinen selbst berechneten Hash und keine Aussage über Inhaltssicherheit.

## RAW-Eignung und technische Integration

Alle Exportaussagen außer dem bereits publizierten RawNIND-ONNX sind
Architekturbewertungen, keine hier erfolgreich ausgeführten Exporte.
CPU/GPU bei PyTorch oder ONNX Runtime bezeichnet nicht automatisch LightCraft-Unterstützung.

| Kandidat | Echter Bayer-RAW-Eingang? | Ausgabe / Einsatz | ONNX und LightCraft-Aufwand | Speicher / erwartbare Laufzeit |
|---|---|---|---|---|
| **RawNIND Bayer** | **Ja**, 4 gepackte Ebenen | 3 Kamera-RGB-Ebenen in doppelter räumlicher Größe; joint denoise+demosaic | ONNX vorhanden, passend zum bestehenden Vertrag und Operatorensatz. Bayer-FP32 beibehalten; Upstream warnt vor FP16-Overflow bei CoreML-ANE/GPU | Ca. 31 MB ONNX, Aktivierungen deutlich größer. Beste belastbare Referenz: historische LightCraft-Messungen unten; nicht neu gemessen |
| **PMRID** | **Ja**, 4→4, k-Sigma-transformiert | Gepacktes Bayer; anschließend inverse Transformation, Unpacking und vorhandenes Demosaicing nötig | Konventioneller PyTorch-Export plausibel. **Nicht aktuell ausführbar**: 5×5/Depthwise-/Stride-Conv, ReLU, Add fehlen; neuer Bayer→Bayer-Vertrag plus Noise-Parameter nötig | Ca. 4,2 MB Checkpoint. Erwartbar günstigstes der hier priorisierten Netze, aber für LightCraft ungemessen. Paper: ~70 ms/MP Snapdragon 855, keine Desktop-/GPU-Prognose |
| **OpenDenoise / SCUNet** | Adapter arbeitet vor Demosaicing; **Netz selbst RGB-trainiert** | Zwei Pseudo-RGB-Pässe [R,G1,B]/[R,G2,B], R/B mitteln, Bayer zurück | Swin-Attention, Norm, MatMul/Softmax, Roll-/Window-Logik, Add und strided Conv; Export mit fixierten Kacheln plausibel, großer Operatorausbau. Nicht einfach 4 Kanäle ins 3-Kanal-Netz geben | ~72 MB Gewichte; Window-Aktivierungen teuer. Autorenadapter nennt ~36 s für 61 MP/2 Pässe auf Radeon Pro V620, ~70 s demosaiced, ~25 s post. Keine LightCraft-Messung |
| **Restormer** | Offizielle Denoising-Gewichte **RGB/Grau**, nicht native Bayer | 3→3; SIDD-sRGB oder Gaussian. Separate LED-RAW-Variante hat andere Lizenz | Depthwise-Conv, Normalisierung, Mul/Add, Attention/MatMul/Softmax, Pixel(Un)Shuffle. Export in generische ONNX-Operatoren plausibel, aktuelle Rust-Executor reichen nicht | Größere Aktivierungen, Tiling nötig; CPU voraussichtlich deutlich teurer als kleines PMRID-Netz. Keine belastbare 24-MP-Zeit hier |
| **KBNet** | Offizielle geprüfte SIDD-Konfiguration 3-Kanal-RGB | RGB/SenseNoise; kein bestätigter 4-Kanal-Bayer-Checkpoint | KBA mit räumlich variierenden Kernbasen/Unfold bzw. eigener Autograd-Funktion: Export-Rewrite/Decomposition wahrscheinlich nötig; kein einfacher Standard-U-Net-Drop-in | Große Patch-/Kernelzwischenpuffer möglich; CPU/GPU erst nach Export und Speicherprofiling bewertbar |
| **Uformer-B** | **Nein**, SIDD-sRGB | 3→3 | Window-Attention, Masken, Norm, GELU, Depthwise-FFN: statische Shapes/Padding prüfen, zusätzlicher Operatorsatz | Aktivierungsspeicher trotz Fensterung hoch; bei großen RAW-Auflösungen kacheln. Laufzeit ungemessen |
| **NAFNet-SIDD** | **Nein**, RGB | 3→3; mögliches eigenes RAW-Retraining ist ein neues Projekt | LayerNorm, Mul/SimpleGate, Depthwise-Conv, Pooling, Residual-Add fehlen. Generischer ONNX-Export plausibel; TLC-/Local-Pooling-Variante separat validieren | Relativ günstiger RGB-Vergleichskandidat, aber keine belegte RAW-Laufzeit; Breite 32/64 beeinflusst Speicher stark |
| **ELD** | **Ja**, sensor-/gainabhängig | RAW-Denoising je Modell; Ausgangsdomäne des exakten Checkpoints vor Adapterbau prüfen | CNN/U-Net prinzipiell exportierbar; Kalibrierung, Exposure-Ratio und Operatorgraph sind zusätzliche Vertragsbestandteile | Kachelung plausibel, Vollbild-/GPU-Zeit nicht gemessen. Cross-Sensor-Qualität offen |
| **SID Sony** | **Ja**, Sony Bayer; Fuji-Modell separat X-Trans | RAW→entwickeltes RGB mit Belichtungsverstärkung; kein neutraler universeller Bayer-Denoiser | TensorFlow→ONNX-Konversion zusätzlicher Schritt. Kamera-/WB-/ISP-Bindung kollidiert mit LightCrafts späterer Farbpipeline | ~84 MB Modell laut Projekt; kein aktueller Laufzeitnachweis. Trainings-RAM-Angaben von 64/128 GB sind **keine** Inferenzanforderungen |
| **LED / UltraLED** | **Ja**, RAW; Architekturen u.a. U-Net/Restormer/NAFNet | Modellabhängig, häufig Extreme-Low-Light und Anpassung an Zielkamera | Technisch relevant, aber Lizenz-Gate schließt kommerzielle Integration ohne Sondererlaubnis aus | Anpassung/Training zusätzlich; keine Produktlaufzeit gemessen |
| **YOND** | **Ja**, RAW mit Noise-Estimation/VST | RAW-Denoising | Nur SNR-Net zu exportieren ersetzt die verschleierten Kernmodule nicht; vollständige Rust-/ONNX-Pipeline nicht nachgewiesen | Iterative/zusätzliche Vorverarbeitung separat messen; derzeit kein verlässlicher Ressourcenplan |
| **GALOSH RAW** | **Ja**, linearer Bayer-Mosaik-Float32-Eingang | Trainingfreie RAW-Verarbeitung; exakten Output-/CFA-Vertrag der Referenz vor Adapterbau sichern | Kein sinnvoller ONNX-Zwang. Eigenständiger Rust-/WGSL-Port nötig; C-FFI/OpenCL-Host passt nicht zu pure Rust. Kachelung benötigt Halo und global konsistente Noise-Schätzung | Keine Gewichte; lineare per-pixel Arbeit und lokale Stufen günstig. CPU/OpenCL/Vulkan-Referenzen vorhanden; keine unabhängige LightCraft-Zeit. Autorenbenchmarks nur Ausgangspunkt |

### Ressourcenbudget statt erfundener Sekundenwerte

Ein FP32-Bayer-Tile [1,4,512,512] benötigt 4 MiB, sein RGB-Output
[1,3,1024,1024] 12 MiB. Schon eine einzelne 32-Kanal-Aktivierung bei 512×512
belegt 32 MiB; mehrere Live-Tensoren, Skip-Verbindungen, im2col und Gewichte kommen dazu.
Deshalb sind Dateigröße oder Parameterzahl keine Peak-RAM-/VRAM-Angabe.

Bei 24 MP benötigt ein voller Kamera-RGB-FP32-Puffer rechnerisch ca. 275 MiB
(24.000.000 × 3 × 4 B), Originalmosaik ca. 92 MiB. Plain/clean/blend und GPU-Kopien
können das mehrfach benötigen. Bestehendes Working-Memory-Budget und maximal vier
gleichzeitige Tile-Aufrufe beibehalten; bei neuen Modellen Peak-RSS/VRAM messen.

LightCraft dokumentiert historische Messungen vom 2026-10-07 auf Ryzen 9 7945HX /
RTX 4090 Laptop (DX12), Modellhash da2750…b901, 24,34 MP/35 Tiles:
ein warmer Tile 1148 ms CPU, 15,1 ms GPU; gesamtes Packing/Blending/Inferenz
5,185 s CPU (16 Worker) bzw. 0,582 s GPU (4 Worker).
Decode, Laden, GPU-Setup, Cache und Export sind ausgeschlossen. Die Zahlen stammen
aus einem älteren Integrationsstand und sind weder neue Benchmarks noch Qualitätsnachweise.
Der aktuelle Bericht übernimmt keine vergleichbare Zeit für andere Modelle. [L1]

## Rangliste der drei besten Kandidaten

Bewertet nach RAW-Eignung, Integration, Lizenznachweis, verfügbarer Vortrainierung und
realistischem Qualitäts-/Ressourcenpotenzial. **Forschungspriorität, keine gemessene Qualitätsrangfolge.**

1. **RawNIND UtNet2 Bayer.** Beste Gesamtpassung: echtes RAW-Training, joint denoise/demosaic,
   existierendes ONNX, exakte Pins und schon implementierte CPU/GPU-Pfade.
   Größter Nutzen liegt in Lizenzzuordnung und Bildqualitätstests statt neuer Infrastruktur.
   CC-BY-Autorenangabe macht die Freigabe aussichtsreicher; konkrete Exportzuordnung bleibt offen.
2. **PMRID.** Beste kleine neuronale Alternative mit tatsächlichen Repository-Gewichten,
   echtem RAW-Training und überschaubarer CNN-Architektur. Nachteil: Sensorrauschmodell/
   k-Sigma, zusätzliche Operatoren und Bayer→Bayer-Domäne. Gewichte-Lizenz schriftlich
   bestätigen; Generalisierung jenseits Reno10x/SID prüfen, bevor erheblicher Portierungsaufwand entsteht.
3. **GALOSH RAW.** Beste nicht-neuronale Ausweichoption: keine Gewichte-/Trainingsrechte,
   Apache-Code, RAW-Eingang und parallele Struktur. Dafür neuer Rust-/WGSL-Port,
   Patentklärung und unabhängige Qualitätsmessung. Nicht als DeepPRIME-Ersatz bezeichnen;
   auch die Autoren benennen Vorteile trainierter Netze in deren Trainingsdomänen.

SCUNet/OpenDenoise fällt hinter diese drei wegen des RGB→RAW-Domain-Mismatch,
doppelter Pässe, Farbverschiebungen und großer Abweichung vom bestehenden Executor.
Restormer, KBNet, Uformer und NAFNet sind nützliche RGB-Referenzen, aber ihre geprüften
fertigen Gewichte erfüllen den nativen Bayer-Vertrag nicht. Ein gutes SIDD-sRGB-Ergebnis
rechtfertigt weder Kanaländerungen ohne Retraining noch eine RAW-Qualitätsbehauptung.

## Isolierter Machbarkeitstest: Ergebnis und Grenzen

**Ausgeführt: statische Artefakt-/Schnittstellenprüfung, kein Modell-Inferenztest.**
In /tmp wurden ausschließlich die über GitHub gelesenen Metadaten und die zulässigen
PMRID-/LightCraft-Quellen abgelegt. Eine Python-Prüfung verglich:

1. SHA-256-digest und Dateigröße aus der RawNIND-Release-API mit known.rs;
2. veröffentlichte RawNIND-Shapes und FP32/opset20 mit dem dokumentierten Vertrag;
3. PMRID-Gruppen-/5×5-Convolution mit den ausdrücklichen Parser-Ausschlüssen.

Tatsächliche Ausgabe:

```text
PASS release SHA-256 metadata matches LightCraft catalog (not a local file hash)
PASS release size = 57700134 bytes matches catalog
PASS published RawNIND shapes: 1x4x512x512 -> 1x3x1024x1024; opset20 FP32
BLOCK PMRID: grouped/5x5 convolution outside LightCraft parser contract
NOT RUN pretrained inference, ONNX export, GPU parity, quality benchmark
```

Die Prüfung bestätigt **keinen** vollständigen ONNX-Graphen oder die numerische
Ausführung eines heruntergeladenen Modells. Das wäre erst nach tatsächlichem Dateiabruf möglich.

**Vollständiger vortrainierter Machbarkeitstest bleibt offen:** Der lokale Git-Clone
scheiterte mit „Failed to connect to proxy port 8080“. Die GitHub-Verbindung funktioniert
für Quelltexte/Metadaten, unterstützt über den verwendeten Fetch-Endpunkt aber keine
Binärdownloads. Lokal fehlen cargo, torch, onnx und onnxruntime; numpy ist vorhanden.
Eine weitere Clone-Anfrage mit zusätzlichem Netzwerkzugriff wurde unterbrochen.
Keine Berechtigung zur Umgehung des Proxys wurde daraus abgeleitet.
Auch `cargo xtask ci` konnte ohne Checkout/Rust-Toolchain nicht ausgeführt werden.
Dies ist ein Dokumentationsbranch; keine produktiven Dateien geändert.

**Konkreter Wiederaufnahmeplan**, isolierter Worktree/Scratch, Download nur manuell
nach Lizenzzuordnung, kein Installer-Automatismus:

- RawNIND-Paket von obiger gepinnter URL einmalig lokal beziehen; SHA-256 und Größe
  gegen Publisher-Pin prüfen, ZIP begrenzt extrahieren, model_bayer.onnx separat hashen.
- Vorhandenen Rust-Parser/Selbsttest und Realmodelltests aus docs/denoise.md verwenden.
  CPU zuerst; Shape, Finite-Werte, Wiederholbarkeit, Gain und synthetische Noise-Reduktion prüfen.
- Vorhandenen ignorierten GPU-Test mit exakt demselben Modell ausführen.
  FP32, maximale absolute/relative Differenz und Hardware/Backend protokollieren;
  bestehende Toleranz aus Test übernehmen, nicht nachträglich aufweiten.
- Danach echte RAW-Crops und vollständige 24-/61-MP-Fotos, Randkacheln, unterschiedliche
  CFA-Orientierungen, ISO, Kameras, HDR/negative Werte und Clipping testen.
  Peak-RSS/VRAM, cold/warm Laden, Packing, Inferenz, Blend und Export getrennt messen.

Beispiele vorhandener Tests (nicht in dieser Sitzung ausgeführt):

```sh
LC_DENOISE_MODEL=/absolute/model_bayer.onnx cargo test --release -p lightcraft-gpu --lib real_model_on_the_gpu_matches_cpu -- --ignored --nocapture
LC_DENOISE_MODEL=/absolute/model_bayer.onnx LC_DENOISE_RAW=/absolute/photo.raw cargo test --release -p lightcraft-engine --features rawnind-model --test it denoise_cpu:: -- --ignored --nocapture
LC_DENOISE_MODEL=/absolute/model_bayer.onnx LC_DENOISE_RAW=/absolute/photo.raw cargo test --release -p lightcraft-engine --features rawnind-model --test it denoise_real:: -- --ignored --nocapture
```

## Freigabefragen und konkrete nächste Schritte

| Priorität | Aufgabe | Konkretes Abnahmekriterium |
|---|---|---|
| P0 | RawNIND-Autorenangabe archivieren und Checkpoint→ONNX-Lizenzzuordnung klären | Wahlrecht GPL-3.0 **oder** CC BY 4.0 sowie obiger Checkpoint/Export ausdrücklich erfasst; Exportzusätze/Notices dokumentiert; keine GPL-Implementierung übernommen |
| P0 | Hashgebundene Lizenzakte | Originaldatei, Version, SHA-256, Rechteinhaber, vollständige Lizenz, Attribution, Exportänderungen und Datum gespeichert; bevor Gewichte weitergegeben werden |
| P0 | Vorhandene RawNIND-Tests auf ausführbarer Umgebung | Parser/Selbsttest/CPU/GPU erfolgreich; keine neue Pipeline; dokumentierte Laufzeit und Peak-Speicher |
| P1 | Qualitätsvergleich mit DeepPRIME | Legale lokale Referenzausgaben derselben RAW-Dateien, gleiche Ausgabeauflösung und nachvollziehbare WB/Farb-/Schärfungseinstellungen; keine DxO-Assets verteilen |
| P1 | PMRID-Gewichte-/Datenbestätigung und kleiner Exportversuch | Checkpointlizenz und SID-Zuordnung geklärt; danach in Scratch Torch→ONNX und Referenzvergleich, Operatorinventar; erst dann Domain-/Kernelentscheidung |
| P2 | GALOSH-Lizenz-/Patentprüfung und Referenzbenchmark | NOTICE/Apache-Patentgrant zur Rechteinhaberschaft eingeordnet; aktuelle Ansprüche/Anmeldestatus separat prüfen; C-Referenz nur isoliert, kein Produkt-FFI |
| P2 | RGB-Modelle nur bei eigenständigem Bedarf | Neuer linear/RGB-Vertrag und freigegebene Gewichte, belegter Nutzen gegenüber bestehender RAW-Verarbeitung; kein stiller Austausch |

Qualitätstest: mindestens mehrere Hersteller/Sensorklassen, Basis-ISO bis sehr hohes ISO,
schwaches und gemischtes Licht, Haut, Haare/Fell, Laub, feine Textilien, Schrift,
Sterne, Chroma-Blotches, gesättigte Highlights und tiefe Schatten.
Ground Truth aus registrierten Low-ISO-/Mehrfachaufnahmen; Test-/Trainingsüberschneidungen
ausschließen. RAW-Metriken nur im passenden RAW-Raum; joint-demosaic-Ausgaben nach
einheitlichem Rendering zusätzlich mit PSNR/SSIM und Farberhaltung bewerten.
Blind bewertete 100%-Crops auf Detailverlust, Halluzination, Maze-/Zipper-Artefakte,
Farbstiche und Kachelnähte prüfen. DxO-Bilder sind Vergleich, kein Ground Truth.
Freigabeschwellen und Testset vor dem Vergleich festlegen.

**Empfohlener nächster Implementierungsschritt: keiner in der produktiven Pipeline.**
Zuerst RawNIND-Lizenzakte und bestehende Tests abschließen. Falls CC-BY-Zuordnung scheitert,
GPL-Release-Policy separat entscheiden oder PMRID-Rechte klären. Eigenes RAW-Training
mit ausdrücklich freigegebenen Daten ist eine Rückfalloption, kein bereits verfügbares
vortrainiertes Ergebnis dieser Recherche.

## Quellen und überprüfte Stände

Alle Quellen am 2026-10-28 geprüft, soweit oben nicht als Abrufblockade bezeichnet.
README-/LICENSE-Links erklären den jeweiligen Befund, keine pauschale Freigabe von
Abhängigkeiten, Daten oder extern gehosteten Gewichten.

- **L1:** [LightCraft docs/denoise.md](https://github.com/storytold/lightcraft/blob/59a5d00c608dd4dc4b3c0d9cd4e914e81aa86ce5/docs/denoise.md), [known.rs](https://github.com/storytold/lightcraft/blob/59a5d00c608dd4dc4b3c0d9cd4e914e81aa86ce5/crates/denoise-core/src/known.rs), [onnx.rs](https://github.com/storytold/lightcraft/blob/59a5d00c608dd4dc4b3c0d9cd4e914e81aa86ce5/crates/denoise/src/onnx.rs).
- **R1:** [RawNIND-Autoren-README, Gewichte-Duallizenz](https://github.com/trougnouf/rawnind_jddc/blob/e80fb7b14dc440bba38c01f312fd3cc02deb566c/README.md), [Code-LICENSE](https://github.com/trougnouf/rawnind_jddc/blob/e80fb7b14dc440bba38c01f312fd3cc02deb566c/LICENSE).
- **R2:** [darktable-ai RawNIND-Modellkarte](https://github.com/darktable-org/darktable-ai/blob/master/models/rawdenoise-nind/README.md), [model.yaml](https://github.com/darktable-org/darktable-ai/blob/master/models/rawdenoise-nind/model.yaml). Gelesene Blob-SHAs: README 9ec0bf11420faded1daff48c7d35d377bd50d7e2; Modellkarte des Exportpakets weiterhin GPL-3.0.
- **R3:** [Release-API mit Publisher-digest](https://api.github.com/repos/darktable-org/darktable-ai/releases/tags/release-5.6.0), [RawNIND-Paper](https://arxiv.org/abs/2501.08924).
- **R4:** [RawNIND-Datensatz DOI](https://doi.org/10.14428/DVN/DEQCIM); direkte Dataverse-Abfrage hier nicht erfolgreich, Datensatzlizenzangabe deshalb aus Autoren-/Modellmetadaten, keine neue Vollprüfung aller Bilder.
- **R5:** [OpenDenoise README](https://github.com/chbornman/OpenDenoise), [LICENSE](https://github.com/chbornman/OpenDenoise/blob/main/LICENSE).
- **R6:** [SCUNet](https://github.com/cszn/SCUNet/tree/52e440a80a655b01e0b41e9dd9bfe599bc11625e), [LICENSE](https://github.com/cszn/SCUNet/blob/52e440a80a655b01e0b41e9dd9bfe599bc11625e/LICENSE), [Downloadskript](https://github.com/cszn/SCUNet/blob/52e440a80a655b01e0b41e9dd9bfe599bc11625e/main_download_pretrained_models.py).
- **R7:** [SCUNet-Paper](https://arxiv.org/abs/2203.13278), [KAIR-Release-API](https://api.github.com/repos/cszn/KAIR/releases/tags/v1.0).
- **R8:** [PMRID](https://github.com/MegEngine/PMRID/tree/8ebb9e8e96559881dee957f34243933c5beb77dd), [LICENSE](https://github.com/MegEngine/PMRID/blob/8ebb9e8e96559881dee957f34243933c5beb77dd/LICENSE), [Torch-Architektur](https://github.com/MegEngine/PMRID/blob/8ebb9e8e96559881dee957f34243933c5beb77dd/models/net_torch.py).
- **R9:** [PMRID-Paper, insbesondere §4.2/4.3](https://arxiv.org/abs/2010.06935).
- **R10:** [Restormer LICENSE.md](https://github.com/swz30/Restormer/blob/68dc6ac472db26f16361150cb7a96a1bc87da93f/LICENSE.md), [Denoising](https://github.com/swz30/Restormer/blob/68dc6ac472db26f16361150cb7a96a1bc87da93f/Denoising/README.md).
- **R11:** [KBNet LICENSE](https://github.com/zhangyi-3/KBNet/blob/9f16c43039360e0551d3bfadb0b0d855ff02b46a/LICENSE), [Denoising](https://github.com/zhangyi-3/KBNet/blob/9f16c43039360e0551d3bfadb0b0d855ff02b46a/Denoising/README.md), [SIDD-Konfiguration](https://github.com/zhangyi-3/KBNet/blob/9f16c43039360e0551d3bfadb0b0d855ff02b46a/Denoising/Options/sidd.yml).
- **R12:** [Uformer LICENSE](https://github.com/ZhendongWang6/Uformer/blob/65fc970a8ffc09605faca74ed016ee93c9ad8a36/LICENSE), [README](https://github.com/ZhendongWang6/Uformer/blob/65fc970a8ffc09605faca74ed016ee93c9ad8a36/README.md), [Architektur](https://github.com/ZhendongWang6/Uformer/blob/65fc970a8ffc09605faca74ed016ee93c9ad8a36/model.py).
- **R13:** [NAFNet Original-README](https://github.com/megvii-research/NAFNet/blob/main/readme.md), [LICENSE einschließlich BasicSR](https://github.com/megvii-research/NAFNet/blob/main/LICENSE).
- **R14:** [ELD Original-README](https://github.com/Vandermode/ELD/blob/master/README.md), [LICENSE](https://github.com/Vandermode/ELD/blob/master/LICENSE).
- **R15:** [SIDD Originalseite, Abschnitt License: Datensatz und Code MIT](https://abdokamel.github.io/sidd/).
- **R16:** [Learning to See in the Dark, README/Lizenz/FAQ](https://github.com/cchen156/Learning-to-See-in-the-Dark), [Original-Downloadskript](https://github.com/cchen156/Learning-to-See-in-the-Dark/blob/master/download_models.py).
- **R17:** [LED Original-README einschließlich Commercial-Licensing-Angabe](https://github.com/Srameo/LED), [Model-Zoo](https://github.com/Srameo/LED/blob/main/docs/pretrained-models.md).
- **R18:** [YOND Original-README mit Obfuscation-Hinweis](https://github.com/fenghansen/YOND_public).
- **R19:** [GALOSH README](https://github.com/luxgrain/GALOSH/blob/11de0593cc8091933ad76d1d6d873f7dee537f15/README.md), [LICENSE](https://github.com/luxgrain/GALOSH/blob/11de0593cc8091933ad76d1d6d873f7dee537f15/LICENSE), [NOTICE](https://github.com/luxgrain/GALOSH/blob/11de0593cc8091933ad76d1d6d873f7dee537f15/NOTICE), [Paper](https://arxiv.org/abs/2607.03768). Patentnummer/-datum sind Autorenangaben, nicht unabhängig im Patentregister verifiziert.
