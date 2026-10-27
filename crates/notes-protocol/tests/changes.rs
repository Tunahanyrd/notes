use notes_protocol::{parse_note_changes, ParseError, MAX_XML_BYTES};

const FIXTURE: &str = include_str!("fixtures/changes.xml");

#[test]
fn source_boolean_whitespace_cannot_become_a_true_deletion() {
    let xml = FIXTURE.replace(
        "<isDeleted>true</isDeleted>",
        "<isDeleted> true </isDeleted>",
    );
    assert_eq!(
        parse_note_changes(&xml),
        Err(ParseError::InvalidField("isDeleted"))
    );
}

#[test]
fn source_numbers_support_lowercase_hex_prefix_without_whitespace_normalization() {
    let xml = FIXTURE
        .replace("1792926000000", "0x7fffffffffffffff")
        .replace(
            "<sdkVersion>19</sdkVersion>",
            "<sdkVersion>0x13</sdkVersion>",
        );
    let page = parse_note_changes(&xml).unwrap();
    assert_eq!(page.changes[0].sync_modified_time, Some(i64::MAX));
    assert_eq!(page.changes[0].sdk_version, Some(19));
    assert_eq!(
        parse_note_changes(&FIXTURE.replace("1792926000000", " 19 ")),
        Err(ParseError::InvalidField("syncModifiedTime"))
    );
}

#[test]
fn source_sdk_integer_is_signed_32_bit() {
    let xml = FIXTURE.replace(
        "<sdkVersion>19</sdkVersion>",
        "<sdkVersion>2147483648</sdkVersion>",
    );
    assert_eq!(
        parse_note_changes(&xml),
        Err(ParseError::InvalidField("sdkVersion"))
    );
}

#[test]
fn preserves_cursor_revision_flags_and_unknown_xml() {
    let page = parse_note_changes(FIXTURE).unwrap();
    assert!(page.has_next);
    assert_eq!(page.last_change_point, "opaque+/cursor=");
    assert_eq!(page.changes.len(), 2);
    let note = &page.changes[0];
    assert_eq!(note.server_id, "synthetic-live-id");
    assert_eq!(note.sync_modified_time, Some(1792926000000));
    assert_eq!(note.is_deleted, Some(false));
    assert_eq!(note.is_locked, Some(true));
    assert_eq!(note.is_encrypted, Some(true));
    assert_eq!(note.is_updated_by_device, Some(false));
    assert_eq!(note.commit_id.as_deref(), Some("0001-A"));
    assert_eq!(note.sdk_version, Some(19));
    assert_eq!(
        note.extensions[0],
        "<futureField><child attribute=\"preserve\">öğe &amp; değer</child></futureField>"
    );
    assert_eq!(page.extensions, ["<futurePageField enabled=\"true\" />"]);
    assert_eq!(page.changes[1].is_deleted, Some(true));
    assert_eq!(page.changes[1].is_locked, None);
}

#[test]
fn accepts_empty_final_page_but_requires_pagination_fields() {
    let empty = "<NoteChangesView><hasNext>false</hasNext><lastChangePoint/></NoteChangesView>";
    let page = parse_note_changes(empty).unwrap();
    assert!(!page.has_next);
    assert!(page.changes.is_empty());
    assert_eq!(
        parse_note_changes("<NoteChangesView/>"),
        Err(ParseError::MissingField("hasNext"))
    );
    assert_eq!(
        parse_note_changes(&empty.replace("false", "true")),
        Err(ParseError::InvalidField("lastChangePoint"))
    );
}

#[test]
fn missing_deletion_flag_is_not_assumed_false() {
    let page = parse_note_changes(&FIXTURE.replace("<isDeleted>true</isDeleted>", "")).unwrap();
    assert_eq!(page.changes[1].is_deleted, None);
}

#[test]
fn rejects_duplicate_pagination_or_deletion_fields_including_case_variants() {
    assert_eq!(
        parse_note_changes(&FIXTURE.replace(
            "<hasNext>true</hasNext>",
            "<hasNext>true</hasNext><HASnext>false</HASnext>"
        )),
        Err(ParseError::DuplicateField("hasNext"))
    );
    assert_eq!(
        parse_note_changes(&FIXTURE.replace(
            "<isDeleted>true</isDeleted>",
            "<isDeleted>true</isDeleted><isDeleted>false</isDeleted>"
        )),
        Err(ParseError::DuplicateField("isDeleted"))
    );
}

#[test]
fn refuses_missing_identity_and_bad_numeric_or_boolean_values() {
    assert_eq!(
        parse_note_changes(&FIXTURE.replace("<serverId>synthetic-deleted-id</serverId>", "")),
        Err(ParseError::MissingField("serverId"))
    );
    assert_eq!(
        parse_note_changes(&FIXTURE.replace("1792926000000", "9223372036854775808")),
        Err(ParseError::InvalidField("syncModifiedTime"))
    );
    assert_eq!(
        parse_note_changes(&FIXTURE.replace(
            "<isDeleted>true</isDeleted>",
            "<isDeleted>possibly</isDeleted>"
        )),
        Err(ParseError::InvalidField("isDeleted"))
    );
}

#[test]
fn foreign_namespace_cannot_override_a_known_field() {
    let xml = FIXTURE.replace("<isDeleted>true</isDeleted>", "<isDeleted>true</isDeleted><other:isDeleted xmlns:other=\"urn:other\">false</other:isDeleted>");
    let page = parse_note_changes(&xml).unwrap();
    assert_eq!(page.changes[1].is_deleted, Some(true));
    assert_eq!(page.changes[1].extensions.len(), 1);
}

#[test]
fn preserves_decoded_scalar_text_across_cdata_and_comments() {
    let xml = FIXTURE.replace("opaque+/cursor=", "a&amp;b<![CDATA[<+>]]><!-- ignored -->z");
    assert_eq!(
        parse_note_changes(&xml).unwrap().last_change_point,
        "a&b<+>z"
    );
}

#[test]
fn rejects_nested_scalar_and_wrong_or_malformed_document() {
    assert_eq!(
        parse_note_changes(&FIXTURE.replace("opaque+/cursor=", "<nested/>")),
        Err(ParseError::InvalidField("lastChangePoint"))
    );
    assert_eq!(
        parse_note_changes("<NotNotes/>"),
        Err(ParseError::WrongRoot)
    );
    assert_eq!(
        parse_note_changes("<NoteChangesView>"),
        Err(ParseError::InvalidXml)
    );
}

#[test]
fn rejects_entity_definitions_without_resolving_local_files() {
    let xml = "<!DOCTYPE NoteChangesView [<!ENTITY secret SYSTEM 'file:///etc/passwd'>]><NoteChangesView><hasNext>false</hasNext><lastChangePoint>&secret;</lastChangePoint></NoteChangesView>";
    assert_eq!(parse_note_changes(xml), Err(ParseError::InvalidXml));
}

#[test]
fn bounds_input_size_and_xml_node_count() {
    assert_eq!(
        parse_note_changes(&" ".repeat(MAX_XML_BYTES + 1)),
        Err(ParseError::TooLarge)
    );
    let xml = format!(
        "<NoteChangesView>{}</NoteChangesView>",
        "<unknown/>".repeat(50_001)
    );
    assert_eq!(parse_note_changes(&xml), Err(ParseError::InvalidXml));
}
#[test]
fn empty_sdk_version_on_server_tombstone_is_unknown_not_zero() {
    let xml = r#"<NoteChangesView><hasNext>true</hasNext><lastChangePoint>opaque</lastChangePoint><changes><serverId>synthetic-deleted</serverId><isDeleted>true</isDeleted><sdkVersion /></changes></NoteChangesView>"#;
    let page = notes_protocol::parse_note_changes(xml).unwrap();
    assert_eq!(page.changes[0].sdk_version, None);
    let duplicate = xml.replace(
        "<sdkVersion />",
        "<sdkVersion /><SDKVERSION>5500</SDKVERSION>",
    );
    assert!(matches!(
        notes_protocol::parse_note_changes(&duplicate),
        Err(notes_protocol::ParseError::DuplicateField("sdkVersion"))
    ));
}
