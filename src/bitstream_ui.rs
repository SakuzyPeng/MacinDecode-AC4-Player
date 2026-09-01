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
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Select a playlist item")
                .size(11.0)
                .color(theme::MUTED),
        );
    });
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
            ui.add(egui::Label::new(RichText::new(value).size(12.0).color(theme::TEXT)).truncate());
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
    let content = compact_content_summary(&field_summary_text(&presentation.summary));
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

fn compact_content_summary(summary: &str) -> String {
    let Some((description, configuration)) =
        summary
            .rsplit_once(" (")
            .and_then(|(description, configuration)| {
                configuration
                    .strip_suffix(')')
                    .map(|configuration| (description, configuration))
            })
    else {
        return summary.strip_suffix(" main").unwrap_or(summary).to_owned();
    };
    let description = description.strip_suffix(" main").unwrap_or(description);
    if configuration == "single_group" {
        description.to_owned()
    } else {
        format!("{description} ({configuration})")
    }
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
                    draw_report(ui, &snapshot.report);
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

const DETAIL_ROW_HEIGHT: f32 = 28.0;
const DETAIL_HEADER_HEIGHT: f32 = 30.0;

enum DetailTableRow {
    Section {
        title: String,
        state_id: egui::Id,
        default_open: bool,
        open: bool,
        depth: u8,
    },
    Field {
        label: String,
        value: String,
        depth: u8,
        alternate: bool,
    },
}

struct DetailTable {
    context: egui::Context,
    rows: Vec<DetailTableRow>,
    field_depth: u8,
    alternate: bool,
}

impl DetailTable {
    fn new(context: &egui::Context) -> Self {
        Self {
            context: context.clone(),
            rows: Vec::new(),
            field_depth: 0,
            alternate: false,
        }
    }

    fn group(
        &mut self,
        title: impl Into<String>,
        state_id: egui::Id,
        default_open: bool,
        depth: u8,
        contents: impl FnOnce(&mut Self),
    ) {
        let open = egui::collapsing_header::CollapsingState::load_with_default_open(
            &self.context,
            state_id,
            default_open,
        )
        .is_open();
        self.rows.push(DetailTableRow::Section {
            title: title.into(),
            state_id,
            default_open,
            open,
            depth,
        });

        if open {
            let previous_depth = self.field_depth;
            self.field_depth = depth + 1;
            contents(self);
            self.field_depth = previous_depth;
        }
    }

    fn field(&mut self, label: impl Into<String>, value: impl Into<String>) {
        self.rows.push(DetailTableRow::Field {
            label: label.into(),
            value: value.into(),
            depth: self.field_depth,
            alternate: self.alternate,
        });
        self.alternate = !self.alternate;
    }
}

fn draw_report(ui: &mut egui::Ui, report: &InspectReport) {
    let rows = collect_report_rows(ui.ctx(), report);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        draw_table_header(ui);
        egui::ScrollArea::vertical()
            .id_salt("bitstream-details-scroll")
            .auto_shrink([false, false])
            .animated(false)
            .show_rows(ui, DETAIL_ROW_HEIGHT, rows.len(), |ui, visible_rows| {
                for row_index in visible_rows {
                    draw_table_row(ui, &rows[row_index]);
                }
            });
    });
}

fn collect_report_rows(context: &egui::Context, report: &InspectReport) -> Vec<DetailTableRow> {
    let mut table = DetailTable::new(context);
    table.group("Audio", egui::Id::new("details-audio"), true, 0, |table| {
        draw_audio(table, report);
    });

    for presentation in &report.presentations {
        table.group(
            format!("Presentation {}", presentation.index),
            egui::Id::new(("details-presentation", presentation.index)),
            true,
            0,
            |table| draw_presentation(table, presentation),
        );
    }

    for substream in &report.audio_substreams {
        table.group(
            format!("Substream {}", substream.index),
            egui::Id::new(("details-substream", substream.index)),
            false,
            0,
            |table| draw_substream(table, substream),
        );
    }

    table.group(
        format!("Issues ({})", report.issues.len()),
        egui::Id::new("details-issues"),
        !report.issues.is_empty(),
        0,
        |table| draw_issues(table, report),
    );
    table.rows
}

fn draw_audio(table: &mut DetailTable, report: &InspectReport) {
    table.field("Codec", &report.stream.codec);
    table.field("Source", source_kind_text(report.source.kind));
    detail_fields(
        table,
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
    table.field("Frames", report.source.frame_count.to_string());
}

fn draw_presentation(table: &mut DetailTable, presentation: &InspectPresentation) {
    detail_fields(
        table,
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
    nested_presentation_groups(table, presentation);
}

fn nested_presentation_groups(table: &mut DetailTable, presentation: &InspectPresentation) {
    let index = presentation.index;
    table.group(
        "Loudness",
        egui::Id::new(("details-loudness", index)),
        false,
        1,
        |table| draw_loudness(table, &presentation.loudness),
    );
    table.group(
        "Dynamic range control",
        egui::Id::new(("details-drc", index)),
        false,
        1,
        |table| draw_drc(table, &presentation.dynamic_range_control),
    );
    table.group(
        "Mixing metadata",
        egui::Id::new(("details-mixing", index)),
        false,
        1,
        |table| draw_mixing(table, &presentation.mixing_metadata),
    );
    table.group(
        "Downmix",
        egui::Id::new(("details-downmix", index)),
        false,
        1,
        |table| draw_downmix(table, &presentation.downmix),
    );
}

fn draw_loudness(table: &mut DetailTable, loudness: &InspectLoudness) {
    detail_fields(
        table,
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

fn draw_drc(table: &mut DetailTable, drc: &InspectDrc) {
    detail_fields(
        table,
        &[
            ("Enhanced AC-3 profile", &drc.enhanced_ac3_profile),
            ("Home theater AVR", &drc.home_theater_avr),
            ("Flat-panel TV", &drc.flat_panel_tv),
            ("Portable speakers", &drc.portable_speakers),
            ("Portable headphones", &drc.portable_headphones),
        ],
    );
}

fn draw_mixing(table: &mut DetailTable, mixing: &InspectMixing) {
    detail_fields(
        table,
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

fn draw_downmix(table: &mut DetailTable, downmix: &InspectDownmix) {
    detail_fields(
        table,
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

fn draw_substream(table: &mut DetailTable, substream: &InspectAudioSubstream) {
    detail_fields(
        table,
        &[
            ("Summary", &substream.summary),
            ("Channel configuration", &substream.channel_configuration),
            ("Channel layout", &substream.channel_layout),
            ("Object coded", &substream.object_coded),
            ("Bit rate", &substream.bit_rate),
        ],
    );
    table.group(
        "Preprocessing",
        egui::Id::new(("details-preprocessing", substream.index)),
        false,
        1,
        |table| draw_preprocessing(table, &substream.preprocessing),
    );
    table.group(
        "Dialogue Enhancement",
        egui::Id::new(("details-dialogue-enhancement", substream.index)),
        false,
        1,
        |table| draw_dialogue_enhancement(table, &substream.dialogue_enhancement),
    );
}

fn draw_preprocessing(table: &mut DetailTable, preprocessing: &InspectPreprocessing) {
    detail_fields(
        table,
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

fn draw_dialogue_enhancement(table: &mut DetailTable, de: &InspectDialogueEnhancement) {
    detail_fields(
        table,
        &[
            ("Enabled", &de.enabled),
            ("Method", &de.method),
            ("Max gain", &de.max_gain),
            ("Channel configuration", &de.channel_configuration),
        ],
    );
}

fn draw_issues(table: &mut DetailTable, report: &InspectReport) {
    if report.issues.is_empty() {
        table.field("Status", "None");
        return;
    }
    for issue in &report.issues {
        table.field(
            format!("{} · {}", issue.severity, issue.code),
            &issue.message,
        );
        let context = issue_context(
            issue.frame_index,
            issue.presentation_id,
            issue.substream_index,
        );
        if !context.is_empty() {
            table.field("Context", context);
        }
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

fn draw_table_header(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), DETAIL_HEADER_HEIGHT),
        egui::Sense::hover(),
    );
    let divider_x = rect.left() + detail_field_width(rect.width());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, theme::SURFACE);
    draw_table_dividers(ui, rect, divider_x);

    draw_cell_label(
        ui,
        table_cell_rect(rect, rect.left() + 12.0, divider_x - 10.0),
        RichText::new("Field")
            .size(12.0)
            .strong()
            .color(theme::TEXT),
    );
    draw_cell_label(
        ui,
        table_cell_rect(rect, divider_x + 12.0, rect.right() - 10.0),
        RichText::new("Value")
            .size(12.0)
            .strong()
            .color(theme::TEXT),
    );
}

fn draw_table_row(ui: &mut egui::Ui, row: &DetailTableRow) {
    match row {
        DetailTableRow::Section {
            title,
            state_id,
            default_open,
            open,
            depth,
        } => draw_section_row(ui, title, *state_id, *default_open, *open, *depth),
        DetailTableRow::Field {
            label,
            value,
            depth,
            alternate,
        } => draw_field_row(ui, label, value, *depth, *alternate),
    }
}

fn draw_section_row(
    ui: &mut egui::Ui,
    title: &str,
    state_id: egui::Id,
    default_open: bool,
    open: bool,
    depth: u8,
) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), DETAIL_ROW_HEIGHT),
        egui::Sense::click(),
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if response.hovered() {
        theme::HOVER
    } else if depth == 0 {
        theme::ACCENT_SOFT
    } else {
        theme::STAGE
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, fill);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, theme::BORDER),
    );

    let mut displayed_open = open;
    if response.clicked() {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            state_id,
            default_open,
        );
        state.toggle(ui);
        state.store(ui.ctx());
        displayed_open = !displayed_open;
    }

    let left = rect.left() + 10.0 + f32::from(depth) * 18.0;
    draw_disclosure_icon(ui, egui::pos2(left + 4.0, rect.center().y), displayed_open);
    draw_cell_label(
        ui,
        table_cell_rect(rect, left + 18.0, rect.right() - 10.0),
        RichText::new(title).size(12.0).strong().color(theme::TEXT),
    );
}

fn draw_disclosure_icon(ui: &egui::Ui, center: egui::Pos2, open: bool) {
    let points = if open {
        vec![
            egui::pos2(center.x - 4.0, center.y - 2.0),
            egui::pos2(center.x + 4.0, center.y - 2.0),
            egui::pos2(center.x, center.y + 3.0),
        ]
    } else {
        vec![
            egui::pos2(center.x - 2.0, center.y - 4.0),
            egui::pos2(center.x + 3.0, center.y),
            egui::pos2(center.x - 2.0, center.y + 4.0),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        theme::TEXT,
        Stroke::NONE,
    ));
}

fn draw_field_row(ui: &mut egui::Ui, label: &str, value: &str, depth: u8, alternate: bool) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), DETAIL_ROW_HEIGHT),
        egui::Sense::hover(),
    );
    let fill = if alternate {
        theme::STAGE
    } else {
        theme::SURFACE
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, fill);

    let divider_x = rect.left() + detail_field_width(rect.width());
    draw_table_dividers(ui, rect, divider_x);
    let label_left = rect.left() + 12.0 + f32::from(depth) * 18.0;
    draw_cell_label(
        ui,
        table_cell_rect(rect, label_left, divider_x - 10.0),
        RichText::new(label).size(11.0).color(theme::MUTED),
    );
    draw_cell_label(
        ui,
        table_cell_rect(rect, divider_x + 12.0, rect.right() - 10.0),
        RichText::new(value).size(11.0).color(theme::TEXT),
    );
}

fn draw_table_dividers(ui: &egui::Ui, rect: egui::Rect, divider_x: f32) {
    ui.painter().line_segment(
        [
            egui::pos2(divider_x, rect.top()),
            egui::pos2(divider_x, rect.bottom()),
        ],
        Stroke::new(1.0, theme::BORDER),
    );
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, theme::BORDER),
    );
}

fn draw_cell_label(ui: &mut egui::Ui, rect: egui::Rect, text: RichText) {
    let clip_rect = ui.clip_rect().intersect(rect);
    let _ = ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.set_clip_rect(clip_rect);
            ui.add(egui::Label::new(text).truncate());
        },
    );
}

fn table_cell_rect(row_rect: egui::Rect, left: f32, right: f32) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(left.min(row_rect.right()), row_rect.top()),
        egui::pos2(right.max(left).min(row_rect.right()), row_rect.bottom()),
    )
}

fn detail_field_width(total_width: f32) -> f32 {
    (total_width * 0.30)
        .clamp(180.0, 500.0)
        .min(total_width * 0.46)
}

fn detail_fields(table: &mut DetailTable, fields: &[(&str, &ReportedField)]) {
    for (label, field) in fields {
        table.field(*label, field_text(field, true));
    }
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
    use super::{compact_content_summary, detail_field_width, object_core_layout, object_profile};

    #[test]
    fn detail_field_column_tracks_the_window_width_with_sensible_limits() {
        for (window_width, expected_field_width) in
            [(400.0, 180.0), (800.0, 240.0), (2_000.0, 500.0)]
        {
            assert!((detail_field_width(window_width) - expected_field_width).abs() < 0.01);
        }
    }

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

    #[test]
    fn compact_content_hides_only_the_main_role_and_redundant_configuration() {
        assert_eq!(
            compact_content_summary("Object-Based main (single_group)"),
            "Object-Based"
        );
        assert_eq!(
            compact_content_summary("Object-Based main (presentation_config_3)"),
            "Object-Based (presentation_config_3)"
        );
        assert_eq!(
            compact_content_summary("Object-Based alternative (single_group)"),
            "Object-Based alternative"
        );
        assert_eq!(compact_content_summary("Main Street"), "Main Street");
    }
}
