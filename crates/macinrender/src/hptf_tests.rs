use super::*;
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn slow_profile_io_leaves_submission_and_output_controls_live() {
    let mut session = Session::new(&Config {
        renderer: RendererSettings {
            binaural: true,
            layout: "4+7+0".into(),
            sofa: String::new(),
            split_lfe: true,
        },
        output: OutputKind::Null,
        device_id: String::new(),
        input_rate: 48_000,
    })
    .unwrap();
    session.reset(1, 0).unwrap();
    session.configure(1, 1, &[7], None).unwrap();
    let control = session.control();
    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("slow.txt");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let (opened, ready) = mpsc::channel();
    let (release, permission) = mpsc::channel();
    let writer_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .unwrap();
        opened.send(()).unwrap();
        let _ = permission.recv_timeout(Duration::from_secs(10));
        file.write_all(b"Preamp: -3 dB\n").unwrap();
    });
    let loader = control.clone();
    let prepare = std::thread::spawn(move || {
        loader.set_hptf(
            &HptfSettings {
                profile: fifo.to_str().unwrap().to_owned(),
                auto_trim: false,
            },
            1,
        )
    });
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    let (finished, progress) = mpsc::channel();
    let producer = std::thread::spawn(move || {
        control.status().unwrap();
        control.hptf_status().unwrap();
        control.volume(0.5).unwrap();
        control.play(false).unwrap();
        control.orientation([0.0; 3]).unwrap();
        let samples = [0.0; 480];
        assert!(
            session
                .submit(&Frame {
                    epoch: 1,
                    generation: 1,
                    start: 0,
                    duration: 480,
                    complete: true,
                    planes: &[Plane {
                        element: 7,
                        samples: &samples
                    }],
                    initial: &[(
                        7,
                        ObjectState {
                            active: true,
                            gain: 1.0,
                            position: Some([0.0, 1.0, 0.0]),
                            head_locked: false
                        }
                    )],
                    updates: &[],
                })
                .unwrap()
        );
        finished.send(()).unwrap();
    });
    // A barrier, not a file-size/timing guess: no profile bytes are available
    // until every producer/control call has had a chance to complete.
    let completed_while_reading = progress.recv_timeout(Duration::from_secs(2)).is_ok();
    release.send(()).unwrap();
    writer.join().unwrap();
    assert_eq!(prepare.join().unwrap(), Ok(true));
    producer.join().unwrap();
    assert!(
        completed_while_reading,
        "profile I/O blocked the producer or output controls"
    );
}
