#[test]
fn controller_facade_exposes_one_management_component() {
    fn accepts_component(_: Option<crate::apm::ManagementServer>) {}
    accepts_component(None);
}
