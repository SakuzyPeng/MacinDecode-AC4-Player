#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "synthetic fixtures have at most 24 channels and 6144 frames"
)]
use super::*;
use crate::decoder::DecoderController;
use apac_core::model::ChannelLayout;
use std::time::{Duration, Instant};

fn hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

fn fixtures() -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(include_str!(
        "../../tests/fixtures/apac-channels.json"
    ))
    .unwrap()["fixtures"]
        .as_array()
        .unwrap()
        .clone()
}

fn caf(cookie: &[u8], packet: &[u8], packets: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, tag: [u8; 4], body: &[u8]) {
        out.extend(tag);
        out.extend((body.len() as u64).to_be_bytes());
        out.extend(body);
    }
    let config = apac_core::Config::parse(cookie).unwrap();
    let info = apac_core::Decoder::new(&config).unwrap();
    let mut out = b"caff\0\x01\0\0".to_vec();
    let mut desc = (info.info().sample_rate_hz as f64).to_be_bytes().to_vec();
    for word in [
        u32::from_be_bytes(*b"apac"),
        0,
        0,
        1024,
        info.info().channel_count,
        0,
    ] {
        desc.extend(word.to_be_bytes());
    }
    chunk(&mut out, *b"desc", &desc);
    chunk(&mut out, *b"kuki", cookie);
    let mut table = u64::from(packets).to_be_bytes().to_vec();
    table.extend((u64::from(packets) * 1024 - 500).to_be_bytes());
    table.extend(300u32.to_be_bytes());
    table.extend(200u32.to_be_bytes());
    let mut size = packet.len();
    let mut varint = vec![(size & 127) as u8];
    size >>= 7;
    while size != 0 {
        varint.insert(0, (size & 127) as u8 | 128);
        size >>= 7;
    }
    for _ in 0..packets {
        table.extend(&varint);
    }
    chunk(&mut out, *b"pakt", &table);
    let mut data = vec![0; 4];
    for _ in 0..packets {
        data.extend(packet);
    }
    chunk(&mut out, *b"data", &data);
    out
}

fn interleave(block: &DecodedSceneBlock, channels: usize) -> Vec<f32> {
    let mut pcm = vec![0.0; block.duration_frames() as usize * channels];
    for (id, samples) in block
        .objects()
        .iter()
        .map(|o| (o.element_id(), o.samples()))
        .chain(block.lfes().iter().map(|o| (o.element_id(), o.samples())))
    {
        for (frame, &sample) in samples.iter().enumerate() {
            pcm[frame * channels + id as usize - 1] = sample;
        }
    }
    pcm
}

fn drain(controller: &mut DecoderController, channels: usize) -> (Vec<f32>, Option<i64>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut pcm = Vec::new();
    let mut first = None;
    loop {
        controller.poll();
        assert_ne!(
            controller.snapshot().phase(),
            DecodePhase::Failed,
            "{:?}",
            controller.snapshot()
        );
        while let Some(block) = controller.try_pop_scene_block() {
            first.get_or_insert(block.start_frame());
            assert_eq!(block.objects().len() + block.lfes().len(), channels);
            pcm.extend(interleave(&block, channels));
        }
        // Decoding and checkpoint indexing finish independently. Callers seek
        // immediately after draining, so EOS alone is not enough to proceed.
        if controller.snapshot().phase() == DecodePhase::EndOfStream
            && let Some(metrics) = controller.snapshot().metrics()
            && !metrics.is_indexing()
        {
            assert!(
                metrics.index_error().is_none(),
                "APAC index failed: {:?}",
                metrics.index_error()
            );
            return (pcm, first);
        }
        assert!(
            Instant::now() < deadline,
            "APAC worker or seek index stalled: {:?}",
            controller.snapshot()
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn discrete_channel_routing_preserves_every_sample_and_both_lfes() {
    for channels in [1, 2, 6, 8, 12, 16, 24] {
        let layout =
            Layout::from_core(&ChannelLayout::discrete(channels).unwrap(), channels).unwrap();
        // An impulse unique to each channel exposes swaps and unwanted mixing.
        let pcm: Vec<_> = (0..channels * channels)
            .map(|i| {
                if i / channels == i % channels {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let block = scene_block(layout, 48_000, 17, &pcm).unwrap();
        assert_eq!(interleave(&block, channels as usize), pcm);
        assert_eq!(
            block.lfes().len(),
            if channels == 24 {
                2
            } else {
                usize::from(channels >= 6)
            }
        );
        if channels == 24 {
            assert_eq!(
                block
                    .lfes()
                    .iter()
                    .map(SceneLfePcm::element_id)
                    .collect::<Vec<_>>(),
                [4, 10]
            );
            assert_eq!(block.objects().len(), 22);
        }
        if channels > 1 {
            let left = block.objects()[0]
                .initial_state()
                .unwrap()
                .position()
                .unwrap();
            assert!(left.x() < 0.0 && left.y() > 0.0);
        }
    }
    let hoa = ChannelLayout::tagged((190 << 16) | 16, 16, None);
    assert!(Layout::from_core(&hoa, 16).unwrap_err().contains("HOA"));
    let mut wrong = ChannelLayout::discrete(16).unwrap();
    wrong.tag = (128 << 16) | 16;
    assert!(Layout::from_core(&wrong, 16).is_err());
}

#[test]
fn caf_worker_matches_core_pcm_and_frame_exact_seeks_for_all_layouts() {
    let directory = tempfile::tempdir().unwrap();
    for fixture in fixtures() {
        let channels = fixture["channels"].as_u64().unwrap() as usize;
        let cookie = hex(fixture["cookie"].as_str().unwrap());
        let packet = hex(fixture["first"].as_str().unwrap());
        let mut decoder = apac_core::Decoder::from_cookie(&cookie).unwrap();
        let mut reference = Vec::new();
        for _ in 0..6 {
            reference.extend(decoder.decode_vec(&packet).unwrap());
        }
        let reference = &reference[300 * channels..reference.len() - 200 * channels];
        // A renamed CAF must still be recognized by its bytes.
        let path = directory.path().join(format!("{channels}.m4a"));
        std::fs::write(&path, caf(&cookie, &packet, 6)).unwrap();
        let media = MediaSource::new(&path);
        let opened = media.open().unwrap();
        assert_eq!(opened.codec().unwrap(), crate::media::MediaCodec::Apac);
        let report = crate::apac::Report::read(Source::new(&opened, Arc::new(|| false))).unwrap();
        assert!(report.summary[0].contains("APAC"));
        let mut controller = DecoderController::new();
        controller.ensure_open(&path);
        let (actual, start) = drain(&mut controller, channels);
        assert_eq!(start, Some(0));
        assert_eq!(bits(&actual), bits(reference), "{channels} channels");
        let duration = (reference.len() / channels) as u64;
        for target in [1025, 33, duration - 1, duration, 0] {
            controller.seek(target).unwrap();
            let (actual, first) = drain(&mut controller, channels);
            assert_eq!(
                bits(&actual),
                bits(&reference[target as usize * channels..]),
                "{channels} channels at {target}"
            );
            assert_eq!(
                first,
                (target < duration).then_some(i64::try_from(target).unwrap())
            );
            assert_eq!(
                controller.snapshot().metrics().unwrap().lfe_count(),
                usize::from(channels >= 6) + usize::from(channels == 24)
            );
        }
    }
}

#[test]
fn apac_mp4_is_detected_and_decoded_and_cancellation_interrupts_input() {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/apac-mp4.json")).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("apac.mp4");
    std::fs::write(&path, hex(data["hex"].as_str().unwrap())).unwrap();
    let opened = MediaSource::new(&path).open().unwrap();
    assert_eq!(opened.codec().unwrap(), crate::media::MediaCodec::Apac);
    assert!(Media::open(Source::new(&opened, Arc::new(|| true))).is_err());
    let media = Media::open(Source::new(&opened, Arc::new(|| false))).unwrap();
    let channels = media.track().channels as usize;
    let mut reference = Playback::open(media).unwrap();
    let mut expected = Vec::new();
    let mut buffer = vec![0.0; 1024 * channels];
    loop {
        let frames = reference.read(&mut buffer).unwrap();
        if frames == 0 {
            break;
        }
        expected.extend_from_slice(&buffer[..frames * channels]);
    }
    let mut controller = DecoderController::new();
    controller.ensure_open(&path);
    let (actual, _) = drain(&mut controller, channels);
    assert_eq!(bits(&actual), bits(&expected));
    assert_eq!(
        controller.snapshot().metrics().unwrap().container(),
        DecodeContainer::ApacMp4
    );
}
