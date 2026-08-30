use eframe::egui::{self, Align, Layout, RichText, Stroke};
use macindecode_ac4_inspect::{
    FieldStatus, InspectAudioSubstream, InspectDialogueEnhancement, InspectDownmix, InspectDrc,
    InspectLoudness, InspectMixing, InspectPreprocessing, InspectPresentation, InspectReport,
    InspectSourceKind, ReportedField,
};

use crate::inspection::{InspectionSnapshot, InspectionState, field_summary_text, field_text};
use crate::model::SelectedSource;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitstreamAction {
    OpenDetails,
    Retry,
}

pub fn draw_card(
    ui: &mut egui::Ui,
    source: Option<&SelectedSource>,
    state: Option<&InspectionState>,
) -> Option<BitstreamAction> {
    match (source, state) {
        (None, _) => draw_empty_card(ui),
        (Some(_), Some(InspectionState::Pending) | None) => draw_pending_card(ui),
        (Some(_), Some(InspectionState::Ready(snapshot))) => draw_ready_card(ui, snapshot),
        (Some(_), Some(InspectionState::Failed(error))) => draw_failed_card(ui, error),
    }
}

fn draw_empty_card(ui: &mut egui::Ui) -> Option<BitstreamAction> {
    summary_rows(ui, ["—", "—", "—"]);
    ui.separator();
    ui.label(
        RichText::new("Select a playlist item")
            .size(11.0)
            .color(theme::MUTED),
    );
    None
}

fn draw_pending_card(ui: &mut egui::Ui) -> Option<BitstreamAction> {
    summary_rows(ui, ["—", "—", "—"]);
    ui.separator();
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(
            RichText::new("Inspecting bitstream…")
                .size(11.0)
                .color(theme::MUTED),
        );
    });
    None
}

fn draw_ready_card(ui: &mut egui::Ui, snapshot: &InspectionSnapshot) -> Option<BitstreamAction> {
    let report = &snapshot.report;
    let presentation = report.presentations.first();
    let values = [
        bit_rate_and_core_text(report),
        content_summary_text(report),
        presentation.map_or_else(
            || "Not present".to_owned(),
            |presentation| {
                format!(
                    "{} · {}",
                    field_summary_text(&presentation.loudness.loudness),
                    field_summary_text(&presentation.loudness.maximum_true_peak)
                )
            },
        ),
    ];
    summary_rows(ui, values.iter().map(String::as_str));
    ui.separator();

    let mut action = None;
    ui.horizontal(|ui| {
        let presentation_count = report.presentations.len();
        let substream_count = report.audio_substreams.len();
        let issue_count = report.issues.len();
        ui.label(
            RichText::new(format!(
                "{presentation_count} P · {substream_count} SS · {issue_count} issues"
            ))
            .size(10.0)
            .color(if issue_count == 0 {
                theme::MUTED
            } else {
                theme::WARNING
            }),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_sized([76.0, 28.0], egui::Button::new("Details…"))
                .clicked()
            {
                action = Some(BitstreamAction::OpenDetails);
            }
        });
    });
    action
}

fn draw_failed_card(ui: &mut egui::Ui, error: &str) -> Option<BitstreamAction> {
    summary_rows(ui, ["—", "—", "—"]);
    ui.separator();

    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Inspection failed")
                .size(11.0)
                .color(theme::WARNING),
        )
        .on_hover_text(error);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_sized([76.0, 28.0], egui::Button::new("Retry"))
                .clicked()
            {
                action = Some(BitstreamAction::Retry);
            }
        });
    });
    action
}

fn summary_rows<'a>(ui: &mut egui::Ui, values: impl IntoIterator<Item = &'a str>) {
    const LABELS: [&str; 3] = ["Bit rate / core", "Content", "Loudness / peak"];
    for (label, value) in LABELS.into_iter().zip(values) {
        compact_key_value(ui, label, value);
    }
}

fn compact_key_value(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.0).color(theme::MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(value).size(12.0).color(theme::TEXT)).truncate())
                .on_hover_text(value);
        });
    });
}

fn preferred_bit_rate(report: &InspectReport) -> &ReportedField {
    if report.stream.bit_rate.status == FieldStatus::Present {
        &report.stream.bit_rate
    } else {
        &report.stream.estimated_average_bit_rate
    }
}

fn bit_rate_and_core_text(report: &InspectReport) -> String {
    let bit_rate = preferred_bit_rate(report);
    let bit_rate_text = field_summary_text(bit_rate);
    if !has_object_coded_audio(report) {
        return bit_rate_text;
    }

    bit_rate
        .value
        .as_ref()
        .and_then(serde_json::Value::as_u64)
        .and_then(object_core_layout)
        .map_or(bit_rate_text.clone(), |layout| {
            format!("{bit_rate_text} · {layout}")
        })
}

fn has_object_coded_audio(report: &InspectReport) -> bool {
    report.audio_substreams.iter().any(|substream| {
        substream.object_coded.status == FieldStatus::Present
            && substream
                .object_coded
                .value
                .as_ref()
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    })
}

fn content_summary_text(report: &InspectReport) -> String {
    let Some(presentation) = report.presentations.first() else {
        return "Not present".to_owned();
    };
    let content = field_summary_text(&presentation.summary).replace(" (single_group)", "");
    if !has_object_coded_audio(report) {
        return content;
    }

    presentation
        .minimal_compatibility_level
        .value
        .as_ref()
        .and_then(serde_json::Value::as_u64)
        .and_then(object_profile)
        .map_or(content.clone(), |profile| format!("{content} · {profile}"))
}

const fn object_profile(compatibility_level: u64) -> Option<&'static str> {
    match compatibility_level {
        3 => Some("16 objects (L3)"),
        4 => Some("20 objects (L4)"),
        _ => None,
    }
}

const fn object_core_layout(bit_rate_kbps: u64) -> Option<&'static str> {
    match bit_rate_kbps {
        768 => Some("5.1.4 (9.1)"),
        1_500 => Some("7.1.4 (11.1)"),
        _ => None,
    }
}

const fn source_kind_text(kind: InspectSourceKind) -> &'static str {
    match kind {
        InspectSourceKind::Mp4 => "MP4",
        InspectSourceKind::AnnexG => "Annex G",
    }
}

pub fn draw_details(
    root: &mut egui::Ui,
    source: &SelectedSource,
    state: Option<&InspectionState>,
) -> Option<BitstreamAction> {
    let mut action = None;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::BACKGROUND)
                .inner_margin(egui::Margin::same(22)),
        )
        .show(root, |ui| {
            details_header(ui, source, state);
            ui.add_space(14.0);
            match state {
                Some(InspectionState::Ready(snapshot)) => {
                    egui::ScrollArea::vertical()
                        .id_salt("bitstream-details-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| draw_report(ui, &snapshot.report));
                }
                Some(InspectionState::Failed(error)) => {
                    action = draw_details_error(ui, error);
                }
                Some(InspectionState::Pending) | None => draw_details_pending(ui),
            }
        });
    action
}

fn details_header(ui: &mut egui::Ui, source: &SelectedSource, state: Option<&InspectionState>) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.heading(RichText::new("Bitstream details").color(theme::TEXT));
            ui.label(
                RichText::new(source.display_name())
                    .size(12.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(
                RichText::new(source.path().display().to_string())
                    .size(10.0)
                    .color(theme::MUTED),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let Some(InspectionState::Ready(snapshot)) = state else {
                return;
            };
            if ui
                .add_sized([108.0, 32.0], egui::Button::new("Copy report"))
                .clicked()
            {
                ui.ctx().copy_text(snapshot.full_text.clone());
            }
        });
    });
}

fn draw_details_pending(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(80.0);
        ui.spinner();
        ui.label(RichText::new("Inspecting bitstream…").color(theme::MUTED));
    });
}

fn draw_details_error(ui: &mut egui::Ui, error: &str) -> Option<BitstreamAction> {
    let mut action = None;
    detail_frame(ui, |ui| {
        ui.label(
            RichText::new("Inspection failed")
                .strong()
                .color(theme::WARNING),
        );
        ui.label(RichText::new(error).size(11.0).color(theme::MUTED));
        ui.add_space(8.0);
        if ui
            .add_sized([96.0, 32.0], egui::Button::new("Retry"))
            .clicked()
        {
            action = Some(BitstreamAction::Retry);
        }
    });
    action
}

fn draw_report(ui: &mut egui::Ui, report: &InspectReport) {
    collapsible_group(ui, "Audio", egui::Id::new("details-audio"), true, |ui| {
        draw_audio(ui, report);
    });
    ui.add_space(8.0);

    for presentation in &report.presentations {
        let title = format!("Presentation {}", presentation.index);
        collapsible_group(
            ui,
            &title,
            egui::Id::new(("details-presentation", presentation.index)),
            true,
            |ui| draw_presentation(ui, presentation),
        );
        ui.add_space(8.0);
    }

    for substream in &report.audio_substreams {
        let title = format!("Substream {}", substream.index);
        collapsible_group(
            ui,
            &title,
            egui::Id::new(("details-substream", substream.index)),
            false,
            |ui| draw_substream(ui, substream),
        );
        ui.add_space(8.0);
    }

    let issues_title = format!("Issues ({})", report.issues.len());
    collapsible_group(
        ui,
        &issues_title,
        egui::Id::new("details-issues"),
        !report.issues.is_empty(),
        |ui| draw_issues(ui, report),
    );
}

fn draw_audio(ui: &mut egui::Ui, report: &InspectReport) {
    plain_detail_row(ui, "Codec", &report.stream.codec);
    plain_detail_row(ui, "Source", source_kind_text(report.source.kind));
    detail_fields(
        ui,
        &[
            ("Track index", &report.source.track_index),
            ("Duration", &report.source.duration),
            ("Bit rate", &report.stream.bit_rate),
            (
                "Estimated average bit rate",
                &report.stream.estimated_average_bit_rate,
            ),
            ("Bitstream version", &report.stream.bitstream_version),
            ("Frame rate", &report.stream.frame_rate),
            ("Sample rate", &report.stream.sample_rate),
            ("I-frame", &report.stream.i_frame),
            ("I-frame interval", &report.stream.i_frame_interval),
            ("Sync word", &report.stream.sync_word),
            ("CRC errors", &report.stream.crc_errors),
            (
                "Number of presentations",
                &report.stream.number_of_presentations,
            ),
            (
                "Number of audio substreams",
                &report.stream.number_of_audio_substreams,
            ),
        ],
    );
    plain_detail_row(ui, "Frames", &report.source.frame_count.to_string());
}

fn draw_presentation(ui: &mut egui::Ui, presentation: &InspectPresentation) {
    detail_fields(
        ui,
        &[
            ("Presentation ID", &presentation.presentation_id),
            ("Summary", &presentation.summary),
            ("Type", &presentation.presentation_type),
            (
                "Minimal compatibility level",
                &presentation.minimal_compatibility_level,
            ),
            (
                "Dialogue normalization",
                &presentation.dialogue_normalization,
            ),
            ("Language", &presentation.language),
            ("Multi-PID", &presentation.multi_pid),
            ("Bit rate", &presentation.bit_rate),
            ("Audio substreams", &presentation.audio_substreams),
            (
                "Metadata authentication ID",
                &presentation.metadata_authentication_id,
            ),
        ],
    );
    nested_presentation_groups(ui, presentation);
}

fn nested_presentation_groups(ui: &mut egui::Ui, presentation: &InspectPresentation) {
    let index = presentation.index;
    collapsible_group(
        ui,
        "Loudness",
        egui::Id::new(("details-loudness", index)),
        false,
        |ui| draw_loudness(ui, &presentation.loudness),
    );
    collapsible_group(
        ui,
        "Dynamic range control",
        egui::Id::new(("details-drc", index)),
        false,
        |ui| draw_drc(ui, &presentation.dynamic_range_control),
    );
    collapsible_group(
        ui,
        "Mixing metadata",
        egui::Id::new(("details-mixing", index)),
        false,
        |ui| draw_mixing(ui, &presentation.mixing_metadata),
    );
    collapsible_group(
        ui,
        "Downmix",
        egui::Id::new(("details-downmix", index)),
        false,
        |ui| draw_downmix(ui, &presentation.downmix),
    );
}

fn draw_loudness(ui: &mut egui::Ui, loudness: &InspectLoudness) {
    detail_fields(
        ui,
        &[
            ("Loudness", &loudness.loudness),
            ("Version", &loudness.version),
            ("Regulation type", &loudness.regulation_type),
            ("Correction type", &loudness.correction_type),
            ("Dialogue Intelligence", &loudness.dialogue_intelligence),
            (
                "Integrated loudness (speech-gated)",
                &loudness.integrated_speech_gated,
            ),
            (
                "Integrated loudness (level-gated)",
                &loudness.integrated_level_gated,
            ),
            ("Maximum true peak", &loudness.maximum_true_peak),
            (
                "Maximum momentary loudness",
                &loudness.maximum_momentary_loudness,
            ),
            ("Loudness range", &loudness.loudness_range),
        ],
    );
}

fn draw_drc(ui: &mut egui::Ui, drc: &InspectDrc) {
    detail_fields(
        ui,
        &[
            ("Enhanced AC-3 profile", &drc.enhanced_ac3_profile),
            ("Home theater AVR", &drc.home_theater_avr),
            ("Flat-panel TV", &drc.flat_panel_tv),
            ("Portable speakers", &drc.portable_speakers),
            ("Portable headphones", &drc.portable_headphones),
        ],
    );
}

fn draw_mixing(ui: &mut egui::Ui, mixing: &InspectMixing) {
    detail_fields(
        ui,
        &[
            ("Main audio ducking level", &mixing.main_audio_ducking_level),
            (
                "Main audio ducking level, Center",
                &mixing.main_audio_ducking_level_center,
            ),
            (
                "Main audio ducking level, Front",
                &mixing.main_audio_ducking_level_front,
            ),
        ],
    );
}

fn draw_downmix(ui: &mut egui::Ui, downmix: &InspectDownmix) {
    detail_fields(
        ui,
        &[
            ("Lo/Ro Center mix gain", &downmix.loro_center_mix_gain),
            ("Lo/Ro Surround mix gain", &downmix.loro_surround_mix_gain),
            ("Lt/Rt Center mix gain", &downmix.ltrt_center_mix_gain),
            ("Lt/Rt Surround mix gain", &downmix.ltrt_surround_mix_gain),
            ("LFE mix info", &downmix.lfe_mix_info),
            ("LFE mix gain", &downmix.lfe_mix_gain),
            ("Preferred downmix", &downmix.preferred_downmix),
        ],
    );
}

fn draw_substream(ui: &mut egui::Ui, substream: &InspectAudioSubstream) {
    detail_fields(
        ui,
        &[
            ("Summary", &substream.summary),
            ("Channel configuration", &substream.channel_configuration),
            ("Channel layout", &substream.channel_layout),
            ("Object coded", &substream.object_coded),
            ("Bit rate", &substream.bit_rate),
        ],
    );
    collapsible_group(
        ui,
        "Preprocessing",
        egui::Id::new(("details-preprocessing", substream.index)),
        false,
        |ui| draw_preprocessing(ui, &substream.preprocessing),
    );
    collapsible_group(
        ui,
        "Dialogue Enhancement",
        egui::Id::new(("details-dialogue-enhancement", substream.index)),
        false,
        |ui| draw_dialogue_enhancement(ui, &substream.dialogue_enhancement),
    );
}

fn draw_preprocessing(ui: &mut egui::Ui, preprocessing: &InspectPreprocessing) {
    detail_fields(
        ui,
        &[
            (
                "Previous mix type 2-channel",
                &preprocessing.previous_mix_type_2channel,
            ),
            (
                "Phase 90 filter info 2-channel",
                &preprocessing.phase90_filter_info_2channel,
            ),
            ("Lo/Ro Center mix gain", &preprocessing.loro_center_mix_gain),
            (
                "Lo/Ro Surround mix gain",
                &preprocessing.loro_surround_mix_gain,
            ),
            (
                "Lo/Ro downmix loudness correction",
                &preprocessing.loro_downmix_loudness_correction,
            ),
            ("Lt/Rt Center mix gain", &preprocessing.ltrt_center_mix_gain),
            (
                "Lt/Rt Surround mix gain",
                &preprocessing.ltrt_surround_mix_gain,
            ),
            (
                "Lt/Rt downmix loudness correction",
                &preprocessing.ltrt_downmix_loudness_correction,
            ),
            ("LFE mix gain", &preprocessing.lfe_mix_gain),
            ("Preferred downmix", &preprocessing.preferred_downmix),
            (
                "Previous downmix type 5-channel",
                &preprocessing.previous_downmix_type_5channel,
            ),
            (
                "Previous upmix type 5-channel",
                &preprocessing.previous_upmix_type_5channel,
            ),
            (
                "Previous upmix type 3/4",
                &preprocessing.previous_upmix_type_3_4,
            ),
            (
                "Previous upmix type 3/2/2",
                &preprocessing.previous_upmix_type_3_2_2,
            ),
            ("Phase 90 filter info", &preprocessing.phase90_filter_info),
            (
                "Surround attenuation known",
                &preprocessing.surround_attenuation_known,
            ),
            (
                "LFE attenuation known",
                &preprocessing.lfe_attenuation_known,
            ),
        ],
    );
}

fn draw_dialogue_enhancement(ui: &mut egui::Ui, de: &InspectDialogueEnhancement) {
    detail_fields(
        ui,
        &[
            ("Enabled", &de.enabled),
            ("Method", &de.method),
            ("Max gain", &de.max_gain),
            ("Channel configuration", &de.channel_configuration),
        ],
    );
}

fn draw_issues(ui: &mut egui::Ui, report: &InspectReport) {
    if report.issues.is_empty() {
        ui.label(RichText::new("None").color(theme::MUTED));
        return;
    }
    for issue in &report.issues {
        detail_frame(ui, |ui| {
            ui.label(
                RichText::new(format!("{} · {}", issue.severity, issue.code))
                    .strong()
                    .color(theme::WARNING),
            );
            ui.label(RichText::new(&issue.message).size(11.0).color(theme::TEXT));
            let context = issue_context(
                issue.frame_index,
                issue.presentation_id,
                issue.substream_index,
            );
            if !context.is_empty() {
                ui.label(RichText::new(context).size(10.0).color(theme::MUTED));
            }
        });
        ui.add_space(6.0);
    }
}

fn issue_context(frame: Option<u64>, presentation: Option<u32>, substream: Option<u32>) -> String {
    let mut parts = Vec::new();
    if let Some(frame) = frame {
        parts.push(format!("frame {frame}"));
    }
    if let Some(presentation) = presentation {
        parts.push(format!("presentation {presentation}"));
    }
    if let Some(substream) = substream {
        parts.push(format!("substream {substream}"));
    }
    parts.join(" · ")
}

fn collapsible_group(
    ui: &mut egui::Ui,
    title: &str,
    id: egui::Id,
    default_open: bool,
    contents: impl FnOnce(&mut egui::Ui),
) {
    egui::CollapsingHeader::new(RichText::new(title).size(13.0).strong().color(theme::TEXT))
        .id_salt(id)
        .default_open(default_open)
        .show(ui, |ui| detail_frame(ui, contents));
}

fn detail_fields(ui: &mut egui::Ui, fields: &[(&str, &ReportedField)]) {
    for (label, field) in fields {
        detail_row(ui, label, &field_text(field, true));
    }
}

fn plain_detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    detail_row(ui, label, value);
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.add_sized(
            [190.0, 18.0],
            egui::Label::new(RichText::new(label).size(11.0).color(theme::MUTED)),
        );
        ui.add(egui::Label::new(RichText::new(value).size(11.0).color(theme::TEXT)).wrap());
    });
}

fn detail_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::same(12))
        .show(ui, contents);
}

#[cfg(test)]
mod tests {
    use super::{object_core_layout, object_profile};

    #[test]
    fn maps_only_confirmed_object_core_bit_rate_tiers() {
        assert_eq!(object_core_layout(768), Some("5.1.4 (9.1)"));
        assert_eq!(object_core_layout(1_500), Some("7.1.4 (11.1)"));
        assert_eq!(object_core_layout(1_024), None);
    }

    #[test]
    fn maps_object_profiles_from_the_reported_compatibility_level() {
        assert_eq!(object_profile(3), Some("16 objects (L3)"));
        assert_eq!(object_profile(4), Some("20 objects (L4)"));
        assert_eq!(object_profile(2), None);
    }
}
