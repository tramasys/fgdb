fn main() {
    glib_build_tools::compile_resources(&["assets"], "assets/fgdb.gresource.xml", "fgdb.gresource");
}
