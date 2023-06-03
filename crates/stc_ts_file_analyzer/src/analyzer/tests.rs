use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use once_cell::sync::Lazy;
use rnode::{NodeIdGenerator, RNode, VisitWith};
use stc_testing::logger;
use stc_ts_ast_rnode::RModule;
use stc_ts_builtin_types::Lib;
use stc_ts_env::{Env, ModuleConfig, Rule};
use stc_ts_storage::Single;
use stc_ts_types::{module_id, Id, ModuleId, Type};
use stc_utils::stack;
use swc_common::{input::SourceFileInput, FileName, Mark, SourceMap, SyntaxContext};
use swc_ecma_ast::EsVersion;
use swc_ecma_parser::{lexer::Lexer, Parser, StringInput, Syntax, TsConfig};
use swc_ecma_transforms::resolver;
use swc_ecma_visit::FoldWith;
use testing::StdErr;
use tracing::Level;

use crate::{
    analyzer::{Analyzer, NoopLoader},
    env::EnvFactory,
    tests::{GLOBALS, MARKS},
};

static ENV: Lazy<Env> = Lazy::new(|| Env::simple(Default::default(), EsVersion::latest(), ModuleConfig::None, &Lib::load("es5")));

/// Single-file tester
pub struct Tester<'a, 'b> {
    cm: Arc<SourceMap>,
    pub analyzer: Analyzer<'a, 'b>,
    pub node_id_gen: NodeIdGenerator,
    pub top_level_mark: Mark,
}

pub fn run_test<F, Ret>(op: F) -> Result<Ret, StdErr>
where
    F: FnOnce(&mut Tester) -> Ret,
{
    ::testing::run_test2(false, |cm, handler| {
        let top_level_mark = Mark::new();
        let top_level_ctxt = SyntaxContext::empty().apply_mark(top_level_mark);

        let mut storage = Single {
            parent: None,
            id: ModuleId::builtin(),
            top_level_ctxt,
            path: Arc::new(FileName::Real(PathBuf::new())),
            is_dts: false,
            info: Default::default(),
        };

        let handler = Arc::new(handler);
        swc_common::GLOBALS.set(&crate::tests::GLOBALS, || {
            let analyzer = Analyzer::root(
                ENV.clone(),
                cm.clone(),
                Default::default(),
                Box::new(&mut storage),
                &NoopLoader,
                None,
            );
            let mut tester = Tester {
                cm: cm.clone(),
                analyzer,
                node_id_gen: Default::default(),
                top_level_mark,
            };
            let ret = op(&mut tester);

            Ok(ret)
        })
    })
}

impl Tester<'_, '_> {
    pub fn parse(&self, name: &str, src: &str) -> RModule {
        swc_common::GLOBALS.set(&GLOBALS, || {
            let fm = self.cm.new_source_file(FileName::Real(name.into()), src.into());

            let lexer = Lexer::new(
                Syntax::Typescript(TsConfig {
                    tsx: false,
                    decorators: true,

                    dts: false,
                    no_early_errors: false,
                    ..Default::default()
                }),
                EsVersion::latest(),
                StringInput::from(&*fm),
                None,
            );
            let mut parser = Parser::new_from(lexer);

            let module = parser
                .parse_module()
                .unwrap()
                .fold_with(&mut resolver(MARKS.unresolved_mark(), self.top_level_mark, true));

            RModule::from_orig(&mut NodeIdGenerator::invalid(), module)
        })
    }
}

pub(crate) fn test_two<F>(left: &str, right: &str, op: F)
where
    F: FnOnce(&mut Analyzer, Type, Type),
{
    testing::run_test2(false, |cm, handler| {
        cm.new_source_file(FileName::Anon, "".to_string());

        let fm = cm.new_source_file(
            FileName::Real(Path::new("test.ts").to_path_buf()),
            format!("type T1 = {}; type T2 = {};", left, right),
        );

        let env = get_env();

        let generator = module_id::ModuleIdGenerator::default();
        let path = Arc::new(fm.name.clone());
        let (module_id, top_level_mark) = generator.generate(&path);

        let mut node_id_gen = NodeIdGenerator::default();
        let mut module = {
            let lexer = Lexer::new(
                Syntax::Typescript(TsConfig { ..Default::default() }),
                EsVersion::Es2021,
                SourceFileInput::from(&*fm),
                None,
            );
            let mut parser = Parser::new_from(lexer);

            parser.parse_module().unwrap()
        };
        module = module.fold_with(&mut resolver(env.shared().marks().unresolved_mark(), top_level_mark, true));
        let span = module.span;
        let module = RModule::from_orig(&mut node_id_gen, module);

        let mut storage = Single {
            parent: None,
            id: module_id,
            top_level_ctxt: SyntaxContext::empty().apply_mark(top_level_mark),
            path,
            is_dts: false,
            info: Default::default(),
        };

        {
            let _stack = stack::start(256);

            // Don't print logs from builtin modules.
            let _tracing = tracing::subscriber::set_default(logger(Level::DEBUG));

            let mut analyzer = Analyzer::root(env, cm, Default::default(), Box::new(&mut storage), &NoopLoader, None);
            module.visit_with(&mut analyzer);

            let top_level_ctxt = SyntaxContext::empty().apply_mark(top_level_mark);

            let t1 = analyzer
                .find_type(&Id::new("T1".into(), top_level_ctxt))
                .expect("type T1 should resolved without an issue")
                .expect("type T1 should exist")
                .next()
                .unwrap()
                .into_owned();
            let t2 = analyzer
                .find_type(&Id::new("T2".into(), top_level_ctxt))
                .expect("type T2 should resolved without an issue")
                .expect("type T2 should exist")
                .next()
                .unwrap()
                .into_owned();

            op(&mut analyzer, t1, t2);
        }

        Ok(())
    })
    .unwrap();
}

fn get_env() -> Env {
    let mut libs = vec![];
    let ls = &["es5"];
    for s in ls {
        libs.extend(Lib::load(s))
    }
    libs.sort();
    libs.dedup();

    Env::simple(
        Rule {
            strict_null_checks: true,
            strict_function_types: true,
            ..Default::default()
        },
        EsVersion::latest(),
        ModuleConfig::None,
        &libs,
    )
}
