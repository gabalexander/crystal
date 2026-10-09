use super::*;

/// Each definition `source` has in `lang`: what it's in and its name, its
/// kind, its lines and the other names it goes by.
fn defs(lang: Lang, source: &str) -> Vec<(String, DefKind, u32, u32, Vec<String>)> {
    let mut parser = Parser::new();
    let outline = outline(lang, source.as_bytes(), &mut parser).expect("a grammar for it");
    let defs = outline.defs.into_iter();
    defs.map(|found| {
        (
            found.segments.join("::"),
            found.kind,
            found.start,
            found.end,
            found.names,
        )
    })
    .collect()
}

fn def(
    name: &str,
    kind: DefKind,
    start: u32,
    end: u32,
) -> (String, DefKind, u32, u32, Vec<String>) {
    (name.to_string(), kind, start, end, Vec::new())
}

#[test]
fn rust_definitions_have_their_exact_lines_and_what_they_re_in() {
    let source = "\
/// Doc.
#[derive(Debug, Parser)]
#[serde(rename_all = \"kebab-case\")]
pub struct Settings<T> {
    #[serde(rename = \"after\")]
    pub stop_idle_after: Option<u64>,
    #[arg(long, short = 'w')]
    wait_for_it: bool,
}
#[derive(Subcommand)]
pub enum Command {
    #[command(name = \"restart\")]
    RestartServer { #[arg(long)] force: bool },
    Wiki(WikiArgs),
}
impl<T: Clone> Settings<T> {
    pub fn new() -> Self {
        todo!()
    }
}
trait Thing { fn go(&self); }
const MAX: usize = 3;
macro_rules! out { () => {} }
mod decl;
#[cfg(test)]
mod tests { fn helper() {} }
";
    let mut wanted = vec![
        def("Settings", DefKind::Struct, 4, 9),
        def("Settings::stop_idle_after", DefKind::Field, 6, 6),
        def("Settings::wait_for_it", DefKind::Field, 8, 8),
        def("Command", DefKind::Enum, 11, 15),
        def("Command::RestartServer", DefKind::Variant, 13, 13),
        def("Command::RestartServer::force", DefKind::Field, 13, 13),
        def("Command::Wiki", DefKind::Variant, 14, 14),
        def("Settings::new", DefKind::Method, 17, 19),
        def("Thing", DefKind::Trait, 21, 21),
        def("Thing::go", DefKind::Method, 21, 21),
        def("MAX", DefKind::Const, 22, 22),
        def("out", DefKind::Macro, 23, 23),
        def("decl", DefKind::Module, 24, 24),
        def("tests", DefKind::Module, 26, 26),
        def("tests::helper", DefKind::Function, 26, 26),
    ];
    wanted[1].4 = vec!["after".into()];
    wanted[2].4 = vec!["wait-for-it".into(), "--wait-for-it".into(), "-w".into()];
    wanted[4].4 = vec!["restart".into()];
    wanted[5].4 = vec!["force".into(), "--force".into()];
    wanted[6].4 = vec!["wiki".into()];
    assert_eq!(defs(Lang::Rust, source), wanted);
    let mut parser = Parser::new();
    let outline = outline(Lang::Rust, source.as_bytes(), &mut parser).unwrap();
    let tested: Vec<&str> = (outline.defs.iter())
        .filter(|found| found.test)
        .map(|found| found.segments.last().unwrap().as_str())
        .collect();
    assert_eq!(tested, ["tests", "helper"]);
    assert!(
        outline
            .defs
            .iter()
            .any(|found| found.decl && found.segments == ["decl"])
    );
    assert_eq!(outline.lines, 26);
}

#[test]
fn go_methods_are_their_receiver_s_and_fields_go_by_their_tags() {
    let source = "\
package engine

type Server struct {
\tName, Alias string `toml:\"name\" json:\"name,omitempty\"`
}

func (s *Server) Serve() error { return nil }

func New() *cobra.Command {
\tcmd := &cobra.Command{Use: \"build [flags]\"}
\tcmd.Flags().StringVarP(&opts.File, \"file\", \"f\", \"\", \"usage\")
\treturn cmd
}

const (
\tA = iota
)
";
    let mut parser = Parser::new();
    let outline = outline(Lang::Go, source.as_bytes(), &mut parser).unwrap();
    assert_eq!(outline.package, ["engine"]);
    let found: Vec<(String, DefKind, u32, Vec<String>)> = (outline.defs.into_iter())
        .map(|found| {
            (
                found.segments.join("."),
                found.kind,
                found.start,
                found.names,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("Server".into(), DefKind::Struct, 3, vec![]),
            (
                "Server.Name".into(),
                DefKind::Field,
                4,
                vec!["Name".into(), "name".into()]
            ),
            (
                "Server.Alias".into(),
                DefKind::Field,
                4,
                vec!["Alias".into(), "name".into()]
            ),
            ("Server.Serve".into(), DefKind::Method, 7, vec![]),
            ("New".into(), DefKind::Function, 9, vec![]),
            ("A".into(), DefKind::Const, 16, vec![]),
            ("build".into(), DefKind::Command, 10, vec!["build".into()]),
            (
                "--file".into(),
                DefKind::Flag,
                11,
                vec!["--file".into(), "-f".into()]
            ),
        ]
    );
}

#[test]
fn python_typescript_java_and_c_definitions_are_read_too() {
    let python = "MAX = 3\nclass Server(Base):\n    port: int = 80\n    def serve(self):\n        def inner(): pass\n";
    assert_eq!(
        defs(Lang::Python, python),
        [
            def("MAX", DefKind::Const, 1, 1),
            def("Server", DefKind::Class, 2, 5),
            def("Server::port", DefKind::Field, 3, 3),
            def("Server::serve", DefKind::Method, 4, 5),
        ]
    );
    let typescript = "export interface Options { name: string }\nexport class Server {\n  static create(): Server { return new Server(); }\n}\nexport const handler = async () => 1;\nenum Color { Red }\n";
    assert_eq!(
        defs(Lang::TypeScript, typescript),
        [
            def("Options", DefKind::Interface, 1, 1),
            def("Options::name", DefKind::Field, 1, 1),
            def("Server", DefKind::Class, 2, 4),
            def("Server::create", DefKind::Method, 3, 3),
            def("handler", DefKind::Function, 5, 5),
            def("Color", DefKind::Enum, 6, 6),
            def("Color::Red", DefKind::Variant, 6, 6),
        ]
    );
    let java = "package com.example;\npublic class Server {\n    private int port;\n    public Server() {}\n    void serve() {}\n}\n";
    assert_eq!(
        defs(Lang::Java, java),
        [
            def("Server", DefKind::Class, 2, 6),
            def("Server::port", DefKind::Field, 3, 3),
            def("Server::serve", DefKind::Method, 5, 5),
        ]
    );
    let cpp = "#define MAX 3\nnamespace engine {\nclass Server {\n    void serve(int x);\n};\nvoid Server::serve(int x) {}\n}\n";
    assert_eq!(
        defs(Lang::Cpp, cpp),
        [
            def("MAX", DefKind::Macro, 1, 1),
            def("engine", DefKind::Module, 2, 7),
            def("engine::Server", DefKind::Class, 3, 5),
            def("engine::Server::serve", DefKind::Method, 4, 4),
            def("engine::Server::serve", DefKind::Method, 6, 6),
        ]
    );
}

#[test]
fn names_are_cased_as_serde_and_clap_write_them() {
    assert_eq!(kebab("RestartServer"), "restart-server");
    assert_eq!(kebab("stop_idle_after"), "stop-idle-after");
    assert_eq!(renamed("stop_idle_after", "camelCase"), "stopIdleAfter");
    assert_eq!(renamed("stop_idle_after", "PascalCase"), "StopIdleAfter");
    assert_eq!(
        renamed("stop_idle_after", "SCREAMING-KEBAB-CASE"),
        "STOP-IDLE-AFTER"
    );
    assert_eq!(
        struct_tag_names("`toml:\"a\" json:\"-\" yaml:\"b,omitempty\"`"),
        ["a", "b"]
    );
}
