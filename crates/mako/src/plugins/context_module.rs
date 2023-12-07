use std::path::PathBuf;
use std::sync::Arc;

use mako_core::anyhow::Result;
use mako_core::glob::glob;
use mako_core::swc_common::{Mark, DUMMY_SP};
use mako_core::swc_ecma_ast::{
    BinExpr, BinaryOp, CallExpr, Expr, ExprOrSpread, Lit, Module, ParenExpr, TplElement,
};
use mako_core::swc_ecma_utils::{member_expr, quote_ident, quote_str, ExprExt, ExprFactory};
use mako_core::swc_ecma_visit::{VisitMut, VisitMutWith};

use super::javascript::{is_commonjs_require, is_dynamic_import};
use crate::compiler::Context;
use crate::load::Content;
use crate::plugin::{Plugin, PluginLoadParam, PluginTransformJsParam};
use crate::resolve::get_module_extensions;

pub struct ContextModulePlugin {}

impl Plugin for ContextModulePlugin {
    fn name(&self) -> &str {
        "context_module"
    }

    fn load(&self, param: &PluginLoadParam, _context: &Arc<Context>) -> Result<Option<Content>> {
        if let (Some(glob_pattern), None) = (
            param
                .request
                .query
                .iter()
                .find_map(|(k, v)| k.eq("glob").then_some(v)),
            param.ext_name,
        ) {
            let glob_pattern = PathBuf::from(param.path.clone()).join(glob_pattern);
            let paths = glob(glob_pattern.to_str().unwrap())?;
            let mut key_values = vec![];

            for path in paths {
                let path = path?;
                let rlt_path = path.strip_prefix(param.path.clone())?;

                // full path `./i18n/zh_CN.json`
                let mut keys = vec![format!("./{}", rlt_path.to_string_lossy())];

                // omit ext `./i18n/zh_CN`
                if let Some(ext) = rlt_path.extension() {
                    if get_module_extensions().contains(&ext.to_string_lossy().to_string()) {
                        keys.push(format!(
                            "./{}",
                            rlt_path.with_extension("").to_string_lossy()
                        ));

                        // entry file `./i18n/`, `./i18n`, `.`, `./`
                        if rlt_path.file_stem().unwrap() == "index" {
                            let entry_paths = rlt_path
                                .parent()
                                .map(|p| {
                                    vec![
                                        format!("./{}", p.to_string_lossy()),
                                        format!("./{}/", p.to_string_lossy()),
                                    ]
                                })
                                .unwrap_or(vec![".".to_string(), "./".to_string()]);

                            keys.extend(entry_paths);
                        }
                    }
                }

                for key in keys {
                    key_values.push(format!(
                        "'{}': () => require('{}')",
                        key,
                        path.to_string_lossy()
                    ));
                }
            }

            let code = format!(
                r#"
const map = {{
    {}
}};

module.exports = (id) => {{
    if (map[id]) return map[id]();
    else {{
        const e = new Error("Cannot find module '" + id + "'");
        e.code = 'MODULE_NOT_FOUND';
        throw e;
    }}
}};
"#,
                key_values.join(",\n")
            );
            Ok(Some(Content::Js(code)))
        } else {
            Ok(None)
        }
    }

    fn transform_js(
        &self,
        param: &PluginTransformJsParam,
        ast: &mut Module,
        _context: &Arc<Context>,
    ) -> Result<()> {
        ast.visit_mut_children_with(&mut ContextModuleVisitor {
            unresolved_mark: param.unresolved_mark,
        });
        Ok(())
    }
}

struct ContextModuleVisitor {
    unresolved_mark: Mark,
}

impl VisitMut for ContextModuleVisitor {
    fn visit_mut_call_expr(&mut self, expr: &mut CallExpr) {
        let commonjs_require = is_commonjs_require(expr, &self.unresolved_mark);
        let dynamic_import = is_dynamic_import(expr);
        let first_non_str_arg = match expr.args.first_mut() {
            Some(ExprOrSpread {
                expr: box Expr::Lit(Lit::Str(_)),
                ..
            }) => None,
            Some(ExprOrSpread { expr, .. }) => Some(expr),
            _ => None,
        };

        if (commonjs_require || dynamic_import) && first_non_str_arg.is_some() {
            if let Some((from, glob)) = try_replace_context_arg(
                &mut *first_non_str_arg.unwrap(),
                false,
            )
            .map(|(prefix, suffix)| (prefix, format!("**/*{}", suffix.unwrap_or("".to_string()),)))
            {
                let ctxt_call_expr = CallExpr {
                    callee: expr.callee.clone(),
                    args: vec![quote_str!(format!("{}?context&glob={}", from, glob)).as_arg()],
                    span: DUMMY_SP,
                    type_args: None,
                };

                if commonjs_require {
                    // require('./i18n' + n) -> require('./i18n?context&glob=**/*')('.' + n)
                    expr.callee = ctxt_call_expr.as_callee();
                } else {
                    // import('./i18n' + n) -> import('./i18n?context&glob=**/*').then(m => m('.' + n))
                    expr.callee = member_expr!(
                        @EXT,
                        DUMMY_SP,
                        ctxt_call_expr.into(),
                        then
                    )
                    .as_callee();
                    expr.args = vec![quote_ident!("m")
                        .as_call(DUMMY_SP, expr.args.clone())
                        .as_expr()
                        .to_owned()
                        .into_lazy_arrow(vec![quote_ident!("m").into()])
                        .as_arg()]
                }
            }
        }
    }
}

/**
 * try to find valid context arg
 * and return prefix, suffix and replace first string literal with `./`
 * why we need to replace with `./` prefix?
 * because the context module map is a relative path map, to reduce bundle size
 */
fn try_replace_context_arg(
    mut o_expr: &mut Expr,
    has_visit_top_bin: bool,
) -> Option<(String, Option<String>)> {
    match &mut o_expr {
        // handle `(...)`
        Expr::Paren(ParenExpr {
            expr: paren_expr, ..
        }) => try_replace_context_arg(paren_expr, has_visit_top_bin)
            .map(|(prefix, suffix)| (prefix, suffix)),

        // handle `'./foo/' + bar`
        Expr::Bin(BinExpr {
            op: BinaryOp::Add,
            right: right_expr,
            left: left_expr,
            ..
        }) => {
            // handle suffix of `'./foo/' + bar + '.ext'`
            try_replace_context_arg(left_expr, true).map(|(prefix, _)| {
                let suffix =
                    if let (Expr::Lit(Lit::Str(str)), false) = (&**right_expr, has_visit_top_bin) {
                        Some(str.value.to_string())
                    } else {
                        None
                    };

                (prefix, suffix)
            })
        }

        // handle prefix of `'./foo/' + bar + '.ext'`
        Expr::Lit(Lit::Str(str)) => {
            let prefix = str.value.to_string();

            // replace first str with relative prefix
            str.value = get_relative_prefix(prefix.clone()).into();
            str.raw = None;

            Some((prefix, None))
        }

        // handle `./foo/${bar}.ext`
        Expr::Tpl(tpl) => {
            if !tpl.exprs.is_empty() {
                let prefix = tpl.quasis.first().unwrap().raw.to_string();
                let mut suffix = None;

                // replace first quasi with relative prefix
                tpl.quasis[0].raw = get_relative_prefix(tpl.quasis[0].raw.to_string()).into();
                tpl.quasis[0].cooked = None;

                // extract suffix
                if tpl.quasis.len() > 1 {
                    if let Some(TplElement { raw, .. }) = tpl.quasis.last() {
                        suffix = Some(raw.to_string());
                    }
                }

                Some((prefix, suffix))
            } else {
                None
            }
        }

        _ => None,
    }
}

fn get_relative_prefix(path: String) -> String {
    if path.ends_with('/') {
        "./".to_string()
    } else {
        ".".to_string()
    }
}