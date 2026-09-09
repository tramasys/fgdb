use super::*;
use crate::debugger::parse_record;

#[test]
fn child_pages_validate_counts_identities_and_continuation() {
    let record = parse_record(
        r#"^done,numchild="1",children=[child={name="root.0",exp="0",numchild="0",value="7",type="int"}],has_more="0""#,
    )
    .unwrap();

    let (children, more) = parse_page(&record, 128).unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].value, "7");
    assert_eq!(more, Some(false));
    assert!(parse_page(&record, 0).is_err());

    let empty = parse_record(r#"^done,numchild="0""#).unwrap();
    assert_eq!(parse_page(&empty, 128).unwrap(), (Vec::new(), None));

    for source in [
        r#"^done,numchild="2",children=[child={name="root.0",exp="0"}]"#,
        r#"^done,numchild="1",children=[]"#,
        r#"^done,numchild="0",children="invalid""#,
        r#"^done,numchild="1",children=[child={exp="0"}]"#,
        r#"^done,numchild="1",children=[child={name="",exp="0"}]"#,
        r#"^done,numchild="2",children=[child={name="root.0"},child={name="root.0"}]"#,
        r#"^done,numchild="0",has_more="maybe""#,
        r#"^done,numchild="-1""#,
        r#"^done,children=[]"#,
    ] {
        assert!(
            parse_page(&parse_record(source).unwrap(), 128).is_err(),
            "{source}"
        );
    }
}

#[test]
#[ignore = "requires GDB and built C fixtures"]
fn live_native_pages_and_null_pointer_updates() {
    use crate::app::test_support::{open_debugger, request};

    let (_debugger, client) = open_debugger("c-array-viewer-target", "c_arrays_ready");
    assert!(request(&client, "-stack-select-frame 1").is_done());
    let root = request(&client, "-var-create array_root * large");
    assert!(root.is_done(), "{root:?}");

    for (from, to, expected_more) in [(0, 128, true), (128, 256, true), (8190, 8192, false)] {
        let record = request(
            &client,
            &format!("-var-list-children --all-values array_root {from} {to}"),
        );

        let (children, more) = parse_page(&record, to - from).unwrap();
        assert_eq!(children.len(), to - from);
        assert_eq!(more, Some(expected_more));
        assert_eq!(children[0].value, (from * 3).to_string());
    }

    let (_debugger, client) =
        open_debugger("c-linked-list-viewer-target", "linked_list_checkpoint");
    let record = request(&client, "-var-create pointer_root * custom_head");
    let mut pointer = crate::debugger::variable_object(&record, "custom_head").unwrap();
    assert!(pointer.can_expand());

    let page = request(
        &client,
        "-var-list-children --all-values pointer_root 0 128",
    );

    let (children, _) = parse_page(&page, 128).unwrap();
    assert!(children.iter().any(|child| child.name == "tailward"));
    assert!(request(&client, "-var-assign pointer_root 0").is_done());

    let record = request(&client, "-var-update --all-values pointer_root");
    let update = crate::debugger::variable_updates(&record)
        .into_iter()
        .find(|update| update.varobj == "pointer_root")
        .unwrap();

    pointer.apply_update(&update);
    assert!(pointer.is_null_pointer());
    assert!(!pointer.can_expand());
}
