use notes_protocol::tree::{
    parse_note_tree, TreeParseError, MAX_TREE_DEPTH, MAX_TREE_ENTITIES, MAX_TREE_EXTENSION_BYTES,
    MAX_TREE_FIELD_BYTES, MAX_TREE_NODES, MAX_TREE_XML_BYTES,
};

const FIXTURE: &str = include_str!("fixtures/tree.xml");

#[test]
fn retains_hierarchy_opaque_fields_and_response_metadata() {
    let tree = parse_note_tree(FIXTURE).unwrap();
    assert_eq!(tree.version, Some(2));
    assert_eq!(tree.encrypted, Some(true));
    assert_eq!(tree.is_stroke_grouped, Some(false));
    let page = &tree.pages[0];
    assert_eq!(page.entity.id, "page-1");
    assert_eq!(page.entity.t, Some(-1));
    assert_eq!(page.entity.h.as_deref(), Some("opaque/page+/="));
    assert_eq!(page.layers[0].entity.t, Some(i64::MAX));
    assert_eq!(page.layers[0].objects[0].id, "stroke-1");
    assert_eq!(tree.content_files[0].id, "media-1");
    assert_eq!(tree.floating_objects[0].t, None);
    assert_eq!(tree.floating_objects[0].h.as_deref(), Some(""));
    assert_eq!(tree.body_text.as_ref().unwrap().id, "text-1");
    assert_eq!(tree.model_name_encoded.as_deref(), Some("c3ludGhldGlj"));
    assert_eq!(tree.commit_id.as_deref(), Some(" 0001-A&B "));
    assert_eq!(tree.is_updated_by_device, Some(false));
}

#[test]
fn preserves_original_namespace_context_and_unknown_attributes() {
    let tree = parse_note_tree(FIXTURE).unwrap();
    assert_eq!(tree.source_xml, FIXTURE);
    assert_eq!(
        tree.pages[0].entity.extensions,
        ["<future:pageExtension enabled=\"true\"/>"]
    );
    assert_eq!(
        tree.extensions,
        ["<future:extension><future:nested/></future:extension>"]
    );
    assert!(roxmltree::Document::parse(&tree.extensions[0]).is_err());
    assert!(roxmltree::Document::parse(&tree.source_xml).is_ok());
}

#[test]
fn empty_tree_and_missing_fields_are_not_invented() {
    let tree = parse_note_tree("<noteTree ver='0'/>").unwrap();
    assert_eq!(tree.version, Some(0));
    assert_eq!(tree.encrypted, None);
    assert!(tree.pages.is_empty());
    let tree = parse_note_tree("<noteTree><bodyText id='text'/></noteTree>").unwrap();
    assert_eq!(tree.version, None);
    let body = tree.body_text.unwrap();
    assert_eq!(body.t, None);
    assert_eq!(body.h, None);
}

#[test]
fn rejects_duplicate_identity_across_entity_kinds() {
    let xml = "<noteTree><pageList><page id='same'/></pageList><contentFileList><contentFile id='same'/></contentFileList></noteTree>";
    assert_eq!(parse_note_tree(xml), Err(TreeParseError::DuplicateIdentity));
    assert_eq!(
        parse_note_tree(&FIXTURE.replace("id=\"stroke-1\"", "id=\"layer-1\"")),
        Err(TreeParseError::DuplicateIdentity)
    );
}

#[test]
fn identity_matching_is_case_sensitive_and_requires_exact_attribute_name() {
    let xml = "<noteTree><floatingObjectList><object id='A'/><object id='a'/></floatingObjectList></noteTree>";
    assert_eq!(parse_note_tree(xml).unwrap().floating_objects.len(), 2);
    for value in ["<bodyText/>", "<bodyText id=''/>", "<bodyText ID='x'/>"] {
        assert_eq!(
            parse_note_tree(&format!("<noteTree>{value}</noteTree>")),
            Err(TreeParseError::MissingField("id"))
        );
    }
}

#[test]
fn duplicate_singletons_and_case_variants_are_rejected() {
    for body in [
        "<pageList/><PAGELIST/>",
        "<contentFileList/><contentFileList/>",
        "<floatingObjectList/><floatingObjectList/>",
        "<commitId>a</commitId><COMMITID>b</COMMITID>",
    ] {
        assert!(matches!(
            parse_note_tree(&format!("<noteTree>{body}</noteTree>")),
            Err(TreeParseError::DuplicateField(_))
        ));
    }
    assert_eq!(
        parse_note_tree("<noteTree ver='1' VER='2'/>"),
        Err(TreeParseError::DuplicateField("ver"))
    );
    assert_eq!(
        parse_note_tree("<noteTree><bodyText id='one'/><bodyText id='two'/></noteTree>"),
        Err(TreeParseError::DuplicateField("bodyText"))
    );
    let xml =
        "<noteTree><pageList><page id='p'><layerList/><layerList/></page></pageList></noteTree>";
    assert_eq!(
        parse_note_tree(xml),
        Err(TreeParseError::DuplicateField("layerList"))
    );
    let xml = "<noteTree><pageList><page id='p'><layerList><layer id='l'><objectList/><objectList/></layer></layerList></page></pageList></noteTree>";
    assert_eq!(
        parse_note_tree(xml),
        Err(TreeParseError::DuplicateField("objectList"))
    );
}

#[test]
fn tree_numeric_attributes_are_decimal_signed_and_not_change_feed_hex() {
    for t in ["0x10", " 16 ", "9223372036854775808", "1.0"] {
        let xml = format!("<noteTree><bodyText id='b' t='{t}'/></noteTree>");
        assert_eq!(
            parse_note_tree(&xml),
            Err(TreeParseError::InvalidField("t"))
        );
    }
    assert_eq!(
        parse_note_tree("<noteTree ver='2147483648'/>"),
        Err(TreeParseError::InvalidField("ver"))
    );
    assert_eq!(
        parse_note_tree("<noteTree ver='0x10'/>"),
        Err(TreeParseError::InvalidField("ver"))
    );
    let tree = parse_note_tree(
        "<noteTree ver='-1'><bodyText id='b' t='-9223372036854775808'/></noteTree>",
    )
    .unwrap();
    assert_eq!(tree.version, Some(-1));
    assert_eq!(tree.body_text.unwrap().t, Some(i64::MIN));
}

#[test]
fn edp_is_distinct_from_note_lock_and_invalid_boolean_is_rejected() {
    let tree = parse_note_tree("<noteTree isEncrypted='true' isLocked='true'/>").unwrap();
    assert_eq!(tree.encrypted, None);
    for flag in [" true ", "1", "possibly", ""] {
        assert_eq!(
            parse_note_tree(&format!("<noteTree encrypted='{flag}'/>")),
            Err(TreeParseError::InvalidField("encrypted"))
        );
    }
    assert_eq!(
        parse_note_tree("<noteTree encrypted='TRUE'/>")
            .unwrap()
            .encrypted,
        Some(true)
    );
}

#[test]
fn foreign_namespace_cannot_inject_known_entities_or_attributes() {
    let xml = "<noteTree xmlns='urn:notes' xmlns:f='urn:other' f:encrypted='true'><f:bodyText id='evil'/><bodyText id='good' f:t='99'/></noteTree>";
    let tree = parse_note_tree(xml).unwrap();
    assert_eq!(tree.encrypted, None);
    assert_eq!(tree.body_text.unwrap().id, "good");
    assert_eq!(tree.extensions.len(), 1);
    let xml = "<noteTree xmlns='urn:notes'><bodyText xmlns='' id='different'/></noteTree>";
    assert!(parse_note_tree(xml).unwrap().body_text.is_none());
}

#[test]
fn nested_scalar_wrong_root_malformed_and_dtd_are_rejected() {
    assert_eq!(
        parse_note_tree("<noteTree><commitId><nested/></commitId></noteTree>"),
        Err(TreeParseError::InvalidField("commitId"))
    );
    assert_eq!(
        parse_note_tree("<NoteResource/>"),
        Err(TreeParseError::WrongRoot)
    );
    assert_eq!(
        parse_note_tree("<noteTree>"),
        Err(TreeParseError::InvalidXml)
    );
    let xml = "<!DOCTYPE noteTree [<!ENTITY x SYSTEM 'file:///synthetic-not-read'>]><noteTree><commitId>&x;</commitId></noteTree>";
    assert_eq!(parse_note_tree(xml), Err(TreeParseError::InvalidXml));
}

#[test]
fn bounds_input_nodes_depth_entities_fields_and_unknown_fragments() {
    assert_eq!(
        parse_note_tree(&" ".repeat(MAX_TREE_XML_BYTES + 1)),
        Err(TreeParseError::LimitExceeded("input bytes"))
    );
    let xml = format!(
        "<noteTree>{}</noteTree>",
        "<unknown/>".repeat(MAX_TREE_NODES as usize + 1)
    );
    assert_eq!(parse_note_tree(&xml), Err(TreeParseError::InvalidXml));
    let xml = format!(
        "<noteTree>{}{}</noteTree>",
        "<x>".repeat(MAX_TREE_DEPTH),
        "</x>".repeat(MAX_TREE_DEPTH)
    );
    assert_eq!(
        parse_note_tree(&xml),
        Err(TreeParseError::LimitExceeded("element depth"))
    );
    let objects: String = (0..=MAX_TREE_ENTITIES)
        .map(|n| format!("<object id='synthetic-{n}'/>"))
        .collect();
    let xml = format!("<noteTree><floatingObjectList>{objects}</floatingObjectList></noteTree>");
    assert_eq!(
        parse_note_tree(&xml),
        Err(TreeParseError::LimitExceeded("entity count"))
    );
    let xml = format!(
        "<noteTree><bodyText id='{}'/></noteTree>",
        "a".repeat(MAX_TREE_FIELD_BYTES + 1)
    );
    assert_eq!(
        parse_note_tree(&xml),
        Err(TreeParseError::LimitExceeded("id"))
    );
    let xml = format!(
        "<noteTree><future>{}</future></noteTree>",
        "x".repeat(MAX_TREE_EXTENSION_BYTES)
    );
    assert_eq!(
        parse_note_tree(&xml),
        Err(TreeParseError::LimitExceeded("extension bytes"))
    );
}
