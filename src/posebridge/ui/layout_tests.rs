//! Headless layout checks: no native window, device access or screenshots.
use super::*;

fn text_rect(output: &egui::FullOutput, label: &str) -> Option<egui::Rect> {
    output.shapes.iter().find_map(|shape| {
        if let egui::Shape::Text(text) = &shape.shape
            && text.galley.job.text == label
        {
            let rect = text.galley.rect.translate(text.pos.to_vec2());
            shape.clip_rect.contains(rect.center()).then_some(rect)
        } else {
            None
        }
    })
}

#[test]
fn prepared_stream_waits_for_its_apply_even_if_readback_was_already_drawn() {
    use crate::posebridge::{Input, configuration::Progress};
    let mut prefs = Preferences {
        device: Device {
            transport: Transport::Usb,
            id: "test-port".into(),
            input: Input::RegisterQuaternion,
            ..Device::default()
        },
        ..Preferences::default()
    };
    let mut panel = Panel {
        settings_device: Some((Transport::Usb, "test-port".into())),
        prepare_stream: true,
        prepare_request: Some(42),
        ..Panel::default()
    };
    let mut snapshot = pb::Controller::new().unwrap().snapshot();
    snapshot.descriptor.device = pb::DeviceObservation {
        valid: true,
        observed_unix_ms: Some(123),
        rate_register: Some(7),
        output_register: Some(0xa4),
        algorithm: Some(pb::AlgorithmMode::NineAxis),
        ..pb::DeviceObservation::default()
    };
    let mut view = View {
        request: 42,
        device: Some(prefs.device.clone()),
        snapshot: Some(snapshot),
        apply: Some(Progress {
            target: Settings::default(),
            verified: vec![],
            step: "Verifying",
            complete: false,
            error: None,
        }),
        ..View::default()
    };
    panel.sync_settings(&mut prefs, &view);
    assert_eq!(prefs.device.input, Input::RegisterQuaternion);
    view.apply.as_mut().unwrap().complete = true;
    panel.sync_settings(&mut prefs, &view);
    assert_eq!(prefs.device.input, Input::Automatic);
    prefs.device.input = Input::RegisterQuaternion;
    panel.prepare_stream = true;
    panel.prepare_request = None;
    panel.sync_settings(&mut prefs, &view);
    assert_eq!(prefs.device.input, Input::RegisterQuaternion);
}

#[test]
fn wide_settings_use_both_columns_and_actions_stay_visible_when_scrolled() {
    for (width, height) in [(1000., 700.), (580., 440.)] {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let service = Service::new();
        let mut prefs = Preferences {
            device: Device {
                transport: Transport::Usb,
                id: "test-port".into(),
                name: "Test sensor".into(),
                mounting: [-2, 1, 3],
                ..Device::default()
            },
            ..Preferences::default()
        };
        let mut panel = Panel {
            tab: Tab::Device,
            settings_device: Some((Transport::Usb, "test-port".into())),
            baseline: Some(Settings::default()),
            draft: Settings {
                six_axis: true,
                ..Settings::default()
            },
            ..Panel::default()
        };
        let mut positions = None;
        for frame in 0..5 {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, height),
                    )),
                    time: Some(f64::from(frame) * 0.1),
                    events: if frame == 3 {
                        vec![
                            egui::Event::PointerMoved(egui::pos2(width * 0.5, height * 0.5)),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point,
                                phase: egui::TouchPhase::Move,
                                delta: egui::vec2(0., -2000.),
                                modifiers: egui::Modifiers::default(),
                            },
                        ]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE.inner_margin(16))
                        .show(root, |ui| {
                            panel.contents(
                                ui,
                                &mut prefs,
                                &HeadSnapshot::default(),
                                &service,
                                true,
                            );
                        });
                },
            );
            if frame < 2 {
                continue;
            }
            let connect = text_rect(&output, "Connect").expect("connection action remains visible");
            let apply = text_rect(&output, "Apply changes").expect("apply action remains visible");
            assert!(connect.bottom() < 100. && apply.bottom() < height);
            if frame == 2 {
                positions = Some((connect, apply));
                if width > 800. {
                    let sensor = text_rect(&output, "Sensor").unwrap();
                    let stream = text_rect(&output, "Stream").unwrap();
                    assert!(stream.left() - sensor.left() > 300.);
                    assert!((stream.top() - sensor.top()).abs() < 2.);
                    assert!(text_rect(&output, "Nine-axis").is_some());
                    assert!(text_rect(&output, "Prepare enhanced data").is_some());
                }
            } else {
                let (before_connect, before_apply) = positions.unwrap();
                assert!((connect.top() - before_connect.top()).abs() < 1.);
                assert!((apply.top() - before_apply.top()).abs() < 1.);
            }
        }
        assert_eq!(
            service.view().request,
            0,
            "drawing must not access the sensor"
        );
    }
}
