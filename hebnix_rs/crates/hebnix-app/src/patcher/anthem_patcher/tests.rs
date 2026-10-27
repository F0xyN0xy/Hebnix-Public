use super::*;

fn fixture_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hebnix-anthem-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}
fn cleanup(root: PathBuf) {
    let root = root.canonicalize().unwrap();
    let temp = std::env::temp_dir().canonicalize().unwrap();
    assert!(root.starts_with(&temp) && root != temp);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn audio_selection_preserves_stereo_frames_and_exact_sample_count() {
    let audio = Audio::new((0..2000).map(|i| i as i16).collect(), 2, 1000);
    let clip = audio.section(0.125, 0.333).unwrap();
    assert_eq!(clip.samples.len(), 666);
    assert_eq!(clip.samples[0], 250);
    assert_eq!(clip.samples[665], 915);
    assert!(audio.section(0.9, 0.2).is_err());
    assert!(audio.section(f64::NAN, 0.2).is_err());
    let root = fixture_root();
    let path = root.join("test.wav");
    fs::write(&path, clip.wav()).unwrap();
    let decoded = Audio::decode(&path).unwrap();
    assert_eq!(decoded.samples, clip.samples);
    assert_eq!(decoded.channels, 2);
    cleanup(root);
}

#[test]
fn selection_at_song_end_uses_whole_frames_without_overrunning() {
    let audio = Audio::new(vec![1000; 2000], 2, 1000);
    // Rounding both a 333.5-frame clip and its complementary start used to
    // request 1001 frames from a 1000-frame song.
    let length = 0.3335;
    let clip = audio
        .section(audio.max_start(length).unwrap(), length)
        .unwrap();
    assert_eq!(clip.samples.len(), 668);
    assert!(audio.section(0.8, length).is_err());
}

#[test]
fn wwise_pcm_has_native_format_channel_configuration_and_aligned_samples() {
    for channels in [1, 2] {
        let audio = Audio::new(vec![1234; channels as usize * 4800], channels, 48000);
        let wem = audio.wem();
        assert_eq!(&wem[20..22], &0xfffeu16.to_le_bytes());
        assert_eq!(&wem[16..20], &24u32.to_le_bytes());
        assert_eq!(&wem[56..60], b"data");
        assert_eq!(Audio::from_pcm_wem(&wem).unwrap().samples, audio.samples);
    }
}

#[test]
fn anthem_grid_has_three_rows_and_unlimited_width_based_columns() {
    assert_eq!(anthem_columns(900.0, 8.0), 5);
    assert_eq!(anthem_columns(2400.0, 8.0), 15);
    assert_eq!(anthem_columns(2400.0, 8.0) * 3, 45);
}

#[test]
fn rejects_malformed_bank_headers_and_unsafe_asset_paths() {
    assert!(Bank::parse(b"BKHD\xff\xff\xff\xff").is_err());
    for name in [
        "../test.bnk",
        "C:\\test.bnk",
        "sub/test.bnk",
        "test.upk",
        "..\\test.bnk",
    ] {
        assert!(file_name(name, "bnk").is_err(), "{name}");
    }
    assert!(file_name("MX_Anthem_WHM2022_03.bnk", "bnk").is_ok());
    assert_eq!(bank::event_id("Play_Anthem_WHM2022_03_Goal"), 2266591382);
}

// Opt-in integration coverage: reads installed assets and writes only isolated
// copies in the OS temp directory. No proprietary assets are checked in.
#[test]
#[ignore = "requires HEBNIX_ANTHEM_COOKED and HEBNIX_ANTHEM_CATALOG"]
fn installed_anthems_scan_patch_decode_and_restore() {
    let installed = PathBuf::from(std::env::var("HEBNIX_ANTHEM_COOKED").unwrap());
    let catalog: Value =
        serde_json::from_slice(&fs::read(std::env::var("HEBNIX_ANTHEM_CATALOG").unwrap()).unwrap())
            .unwrap();
    let entries = scan(catalog, &installed).unwrap();
    let supported = entries.iter().filter(|a| a.error.is_none()).count();
    let mut failures = BTreeMap::<String, usize>::new();
    for a in &entries {
        if let Some(error) = &a.error {
            *failures.entry(error.clone()).or_default() += 1;
        }
    }
    println!(
        "Supported {supported}/{} installed anthem products; errors: {failures:?}",
        entries.len()
    );
    for a in entries.iter().filter(|a| a.error.is_some()).take(12) {
        println!("Unavailable: {} ({}): {:?}", a.name, a.upk, a.error);
    }
    let sample = entries
        .iter()
        .find(|a| a.upk.eq_ignore_ascii_case("anthem_whm2022_03_SF.upk"))
        .unwrap();
    let breathe = entries
        .iter()
        .find(|a| {
            a.internal
                .eq_ignore_ascii_case("anthem_badcomputer_breathe")
        })
        .unwrap();
    assert!(
        breathe.error.is_none(),
        "Breathe must remain editable without its original stream: {:?}",
        breathe.error
    );
    for r in &breathe.regions {
        println!(
            "Breathe {}: start={} duration={} original={}",
            r.label, r.clip.start, r.clip.duration, r.preview_available
        );
    }
    assert!(sample.error.is_none(), "{:?}", sample.error);
    assert_eq!(sample.regions.len(), 5);
    for r in &sample.regions {
        println!(
            "{}: {:.6}s (source {} at {:.6}s)",
            r.label, r.clip.duration, r.clip.source, r.clip.start
        );
    }
    let root = fixture_root();
    let cooked = root.join("game/TAGame/CookedPCConsole");
    fs::create_dir_all(&cooked).unwrap();
    let bin = root.join("game/Binaries/Win64");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("RocketLeague.exe"), b"fixture game build").unwrap();
    // Reproduce the reported Breathe case: 30 seconds is long enough for
    // Goal but shorter than MVP, and the original WEM is absent.
    fs::copy(installed.join(&breathe.upk), cooked.join(&breathe.upk)).unwrap();
    for region in &breathe.regions {
        fs::copy(installed.join(&region.bank), cooked.join(&region.bank)).unwrap();
    }
    let breathe_source = root.join("breathe-import.wav");
    fs::write(
        &breathe_source,
        Audio::new(vec![5000; 30_000], 1, 1000).wav(),
    )
    .unwrap();
    let goal = breathe.regions.iter().find(|r| r.label == "Goal").unwrap();
    let goal_start = 30.0 - goal.clip.duration;
    let breathe_patch = SavedPatch {
        name: breathe.name.clone(),
        upk: breathe.upk.clone(),
        internal: breathe.internal.clone(),
        source: breathe_source.to_string_lossy().into_owned(),
        starts: BTreeMap::from([("Goal".into(), goal_start)]),
    };
    let breathe_saved = BTreeMap::from([(breathe.key.clone(), breathe_patch)]);
    apply(
        &cooked,
        HashMap::from([(breathe.key.clone(), breathe.name.clone())]),
        breathe_saved.clone(),
    )
    .unwrap();
    for region in &breathe.regions {
        let bank = Bank::parse(&fs::read(cooked.join(&region.bank)).unwrap()).unwrap();
        let clip = bank
            .clips(&region.event)
            .unwrap()
            .into_iter()
            .find(|c| c.track == region.clip.track)
            .unwrap();
        assert_eq!(clip.source == region.clip.source, region.label != "Goal");
        if region.label == "Goal" {
            let decoded = root.join("breathe-goal.wav");
            fs::write(&decoded, bank.embedded(clip.source).unwrap()).unwrap();
            assert!(
                Audio::from_pcm_wem(&fs::read(&decoded).unwrap())
                    .unwrap()
                    .samples
                    .iter()
                    .all(|s| *s == 5000)
            );
        }
    }
    apply(&cooked, HashMap::new(), breathe_saved).unwrap();
    for region in &breathe.regions {
        assert_eq!(
            fs::read(cooked.join(&region.bank)).unwrap(),
            fs::read(installed.join(&region.bank)).unwrap()
        );
    }
    fs::copy(installed.join(&sample.upk), cooked.join(&sample.upk)).unwrap();
    let mut originals = BTreeMap::new();
    for r in &sample.regions {
        let b = fs::read(installed.join(&r.bank)).unwrap();
        fs::write(cooked.join(&r.bank), &b).unwrap();
        originals.insert(r.bank.clone(), b);
        fs::copy(
            installed.join(format!("{}.wem", r.clip.source)),
            cooked.join(format!("{}.wem", r.clip.source)),
        )
        .unwrap();
    }
    if let Ok(decoder) = std::env::var("HEBNIX_ANTHEM_VGMSTREAM") {
        let tool_dir = root.join("tools/vgmstream-r2168");
        fs::create_dir_all(&tool_dir).unwrap();
        for entry in fs::read_dir(Path::new(&decoder).parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
        {
            if entry.path().is_file() {
                fs::copy(entry.path(), tool_dir.join(entry.file_name())).unwrap();
            }
        }
        let original = audio::original_audio(
            &root,
            &installed.join(format!("{}.wem", sample.regions[0].clip.source)),
        )
        .unwrap();
        for region in &sample.regions {
            let clip = original
                .section(region.clip.start, region.clip.duration)
                .unwrap();
            assert!(
                clip.samples.iter().any(|s| s.unsigned_abs() > 100),
                "{} preview has no audio",
                region.label
            );
        }
    }
    let source = Audio::new(
        (0..44100 * 2 * 40)
            .map(|i| if i % 80 < 40 { 6000 } else { -6000 })
            .collect(),
        2,
        44100,
    );
    let path = root.join("source.wav");
    fs::write(&path, source.wav()).unwrap();
    let starts = sample
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| (r.label.clone(), i as f64))
        .collect();
    let patch = SavedPatch {
        name: sample.name.clone(),
        upk: sample.upk.clone(),
        internal: sample.internal.clone(),
        source: path.to_string_lossy().into_owned(),
        starts,
    };
    let saved = BTreeMap::from([(sample.key.clone(), patch)]);
    let active = HashMap::from([(sample.key.clone(), sample.name.clone())]);
    apply(&cooked, active, saved.clone()).unwrap_or_else(|e| panic!("Apply: {e}"));
    for r in &sample.regions {
        let bank = Bank::parse(&fs::read(cooked.join(&r.bank)).unwrap()).unwrap();
        let c = bank
            .clips(&r.event)
            .unwrap()
            .into_iter()
            .find(|c| c.track == r.clip.track)
            .unwrap();
        assert_ne!(c.source, r.clip.source);
        assert!((c.duration - r.clip.duration).abs() < 0.000001);
        let path = root.join(format!("{}.wav", c.source));
        fs::write(&path, bank.embedded(c.source).unwrap()).unwrap();
        let clip = Audio::from_pcm_wem(&fs::read(&path).unwrap()).unwrap();
        assert!((clip.duration() - r.clip.duration).abs() <= 1.0 / clip.rate as f64);
        let expected = source
            .section(saved[&sample.key].starts[&r.label], r.clip.duration)
            .unwrap();
        assert_eq!(clip.samples, expected.samples);
        if let Ok(decoder) = std::env::var("HEBNIX_ANTHEM_VGMSTREAM") {
            let wem = path.with_extension("wem");
            fs::copy(&path, &wem).unwrap();
            let decoded = root.join(format!("{}-decoded.wav", c.source));
            let output = std::process::Command::new(decoder)
                .args(["-i", "-o"])
                .arg(&decoded)
                .arg(&wem)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(Audio::decode(&decoded).unwrap().samples, expected.samples);
        }
    }
    // Reapplying always starts from the pristine bank, so it never grows.
    let sizes: Vec<_> = originals
        .keys()
        .map(|n| fs::metadata(cooked.join(n)).unwrap().len())
        .collect();
    apply(
        &cooked,
        HashMap::from([(sample.key.clone(), sample.name.clone())]),
        saved.clone(),
    )
    .unwrap();
    assert_eq!(
        sizes,
        originals
            .keys()
            .map(|n| fs::metadata(cooked.join(n)).unwrap().len())
            .collect::<Vec<_>>()
    );
    let mut overlapping = saved.clone();
    overlapping.insert("overlap".into(), saved[&sample.key].clone());
    assert!(
        apply(
            &cooked,
            HashMap::from([
                (sample.key.clone(), sample.name.clone()),
                ("overlap".into(), "overlap".into())
            ]),
            overlapping
        )
        .is_err()
    );
    assert_eq!(
        sizes,
        originals
            .keys()
            .map(|n| fs::metadata(cooked.join(n)).unwrap().len())
            .collect::<Vec<_>>()
    );
    apply(&cooked, HashMap::new(), saved.clone()).unwrap();
    for (name, original) in &originals {
        assert_eq!(&fs::read(cooked.join(&name)).unwrap(), original);
        assert_eq!(&fs::read(installed.join(name)).unwrap(), original);
    }
    // Distinct anthems in a shared bank must compose, then restore independently.
    let mut grouped = BTreeMap::<String, Vec<&Anthem>>::new();
    for anthem in entries.iter().filter(|a| a.error.is_none()) {
        grouped
            .entry(anthem.regions[0].bank.clone())
            .or_default()
            .push(anthem);
    }
    let Some(pair) = grouped.values().find(|group| group.len() >= 2) else {
        println!(
            "No fully installed shared anthem bank; shared composition needs a complete shared-bank fixture"
        );
        cleanup(root);
        return;
    };
    let shared_source = root.join("shared-source.wav");
    fs::write(
        &shared_source,
        Audio::new(vec![6000; 1000 * 600], 1, 1000).wav(),
    )
    .unwrap();
    let mut saved_shared = saved;
    let mut active = HashMap::new();
    for a in &pair[..2] {
        fs::copy(installed.join(&a.upk), cooked.join(&a.upk)).unwrap();
        for r in &a.regions {
            fs::copy(installed.join(&r.bank), cooked.join(&r.bank)).unwrap();
            let media = installed.join(format!("{}.wem", r.clip.source));
            if media.is_file() {
                fs::copy(media, cooked.join(format!("{}.wem", r.clip.source))).unwrap();
            }
        }
        saved_shared.insert(
            a.key.clone(),
            SavedPatch {
                name: a.name.clone(),
                upk: a.upk.clone(),
                internal: a.internal.clone(),
                source: shared_source.to_string_lossy().into_owned(),
                starts: a.regions.iter().map(|r| (r.label.clone(), 0.0)).collect(),
            },
        );
        active.insert(a.key.clone(), a.name.clone());
    }
    apply(&cooked, active.clone(), saved_shared.clone()).unwrap();
    active.remove(&pair[0].key);
    apply(&cooked, active, saved_shared.clone()).unwrap();
    for (i, a) in pair[..2].iter().enumerate() {
        for r in &a.regions {
            let bank = Bank::parse(&fs::read(cooked.join(&r.bank)).unwrap()).unwrap();
            let clip = bank
                .clips(&r.event)
                .unwrap()
                .into_iter()
                .find(|c| c.track == r.clip.track)
                .unwrap();
            assert_eq!(clip.source == r.clip.source, i == 0);
        }
    }
    apply(&cooked, HashMap::new(), saved_shared).unwrap();
    for r in &pair[0].regions {
        assert_eq!(
            fs::read(cooked.join(&r.bank)).unwrap(),
            fs::read(installed.join(&r.bank)).unwrap()
        );
    }
    cleanup(root);
}

#[test]
fn cards_select_anthem_before_showing_import_and_timeline() {
    let root = fixture_root();
    let config = Config::default();
    let mut state = AnthemPatcherState::new(&root, &config);
    let region = Region {
        label: "Goal".into(),
        event: "Play_Breathe_Goal".into(),
        bank: "test.bnk".into(),
        clip: Clip {
            track: 1,
            source: 2,
            start: 0.0,
            duration: 24.451303,
            play_at: 0.0,
            playlist_offset: 23,
        },
        preview_available: false,
    };
    let mut mvp = region.clone();
    mvp.label = "MVP".into();
    mvp.clip.track = 3;
    mvp.clip.duration = 44.153154;
    state.entries.push(Anthem {
        key: "breathe".into(),
        product_id: Some(5538),
        name: "Breathe".into(),
        upk: "breathe.upk".into(),
        internal: "breathe".into(),
        thumbnail: "breathe_T.upk".into(),
        regions: vec![region, mvp],
        error: None,
    });
    state.error = Some("Old audio selection error".into());
    let ctx = egui::Context::default();
    egui_extras::install_image_loaders(&ctx);
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut config = config;
    let mut frame = |state: &mut AnthemPatcherState, events| {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ui| state.render_tab(ui, &root, &tx, &mut config, &HashSet::new()),
        )
    };
    let text_rects = |output: &egui::FullOutput| {
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(text) => {
                    Some((text.galley.text().to_string(), text.visual_bounding_rect()))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    frame(&mut state, vec![]);
    let output = frame(&mut state, vec![]);
    let labels = text_rects(&output);
    assert!(labels.iter().any(|(s, _)| s == "Breathe"));
    assert!(!labels.iter().any(|(s, _)| s == "Import Audio"));
    let pos = labels
        .iter()
        .find(|(s, _)| s == "Select")
        .unwrap()
        .1
        .center();
    let click = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(
        &mut state,
        vec![egui::Event::PointerMoved(pos), click(true)],
    );
    frame(&mut state, vec![click(false)]);
    assert!(state.editor.is_some());
    assert!(state.error.is_none());
    let output = frame(&mut state, vec![]);
    assert!(text_rects(&output).iter().any(|(s, _)| s == "Import Audio"));
    state.editor.as_mut().unwrap().audio = Some(Audio::new(vec![3000; 30_000], 1, 1000));
    let editor = state.editor.as_mut().unwrap();
    assert!(editor.patch_starts(editor.audio.as_ref().unwrap()).is_err());
    editor.audio = Some(Audio::new(vec![3000; 60_000], 1, 1000));
    editor.start = 5.0;
    let starts = editor.patch_starts(editor.audio.as_ref().unwrap()).unwrap();
    assert_eq!(starts.len(), 2);
    assert!(starts.values().all(|v| *v == 5.0));
    assert_eq!(editor.longest_region().unwrap().label, "MVP");
    let output = frame(&mut state, vec![]);
    let labels = text_rects(&output);
    assert!(labels.iter().any(|(s, _)| s == "Play"));
    assert!(labels.iter().any(|(s, _)| s == "Start"));
    assert!(labels.iter().any(|(s, _)| s == "Save and patch anthem"));
    drop(frame);
    drop(state);
    cleanup(root);
}

#[test]
fn timeline_handles_keep_duration_and_scrubber_seeks() {
    let ctx = egui::Context::default();
    let audio = Audio::new(vec![2000; 100_000], 1, 1000);
    let mut start = 15.0;
    let mut cursor = 20.0;
    let length = 24.451303;
    let mut rect = egui::Rect::NOTHING;
    let mut frame = |events: Vec<egui::Event>| {
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                rect = egui::Rect::from_min_size(
                    ui.available_rect_before_wrap().min,
                    egui::vec2(ui.available_width(), 150.0),
                );
                timeline(ui, &audio, &mut start, length, &mut cursor);
            },
        );
        (start, cursor, rect)
    };
    let (_, _, rect) = frame(vec![]);
    frame(vec![]);
    let point = |time: f64, y: f32| {
        egui::pos2(
            rect.left() + rect.width() * (time / 100.0) as f32,
            rect.top() + y,
        )
    };
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    // Drag the stop handle 10 seconds later; the start must move with it.
    let end = point(15.0 + length, 9.0);
    frame(vec![egui::Event::PointerMoved(end), button(end, true)]);
    let end = point(25.0 + length, 9.0);
    let (moved, _, _) = frame(vec![egui::Event::PointerMoved(end)]);
    frame(vec![button(end, false)]);
    assert!((moved - 25.0).abs() < 0.01, "start {moved}");
    // Click outside the selected area; scrub position clamps to its bounds.
    let pos = point(90.0, 20.0);
    frame(vec![egui::Event::PointerMoved(pos), button(pos, true)]);
    let (_, cursor, _) = frame(vec![button(pos, false)]);
    assert!((cursor - (25.0 + length)).abs() < 0.01);
}
