use mako_core::swc_common::errors::Handler;
use mako_core::swc_ecma_ast::{Expr, MemberExpr, MemberProp};
use mako_core::swc_ecma_visit::{Visit, VisitWith};
use mako_core::{anyhow, swc_ecma_ast};
use swc_core::common::Mark;

use crate::plugin::Plugin;

pub struct InvalidSyntaxPlugin {}

impl Plugin for InvalidSyntaxPlugin {
    fn name(&self) -> &str {
        "invalid_syntax"
    }

    fn transform_js(
        &self,
        param: &crate::plugin::PluginTransformJsParam,
        ast: &mut swc_ecma_ast::Module,
        _context: &std::sync::Arc<crate::compiler::Context>,
    ) -> anyhow::Result<()> {
        // 先用白名单的形式，等收集的场景多了之后再考虑通用方案
        // 1、react-loadable/lib/index.js 里有用 __webpack_modules__ 来判断 isWebpackReady
        // 2、...
        if param.path.contains("node_modules") && param.path.contains("react-loadable") {
            return Ok(());
        }
        ast.visit_with(&mut InvalidSyntaxVisitor {
            unresolved_mark: param.unresolved_mark,
            handler: param.handler,
            path: param.path,
        });
        Ok(())
    }
}

pub struct InvalidSyntaxVisitor<'a> {
    unresolved_mark: Mark,
    pub handler: &'a Handler,
    pub path: &'a str,
}

impl<'a> Visit for InvalidSyntaxVisitor<'a> {
    fn visit_member_expr(&mut self, expr: &swc_ecma_ast::MemberExpr) {
        let is_require_ensure =
            is_member_prop(expr, "require", "ensure", true, self.unresolved_mark);
        if is_require_ensure {
            self.handler
                .struct_span_err(expr.span, "require.ensure syntax is not supported yet")
                .emit();
        } else {
            expr.visit_children_with(self);
        }
    }
    fn visit_ident(&mut self, n: &swc_ecma_ast::Ident) {
        // why keep __webpack_nonce__? since styled-components is using it
        let is_webpack_prefix = n.sym.starts_with("__webpack_") && &n.sym != "__webpack_nonce__";
        let has_binding = n.span.ctxt.outer() != self.unresolved_mark;
        if is_webpack_prefix && !has_binding {
            self.handler
                .struct_span_err(
                    n.span,
                    format!("{} syntax is not supported yet", n.sym).as_str(),
                )
                .emit();
        } else {
            n.visit_children_with(self);
        }
    }
}

fn is_member_prop(
    expr: &swc_ecma_ast::MemberExpr,
    obj: &str,
    prop: &str,
    check_obj_binding: bool,
    unresolved_mark: Mark,
) -> bool {
    if let MemberExpr {
        obj: box Expr::Ident(ident),
        prop: MemberProp::Ident(prop_ident),
        ..
    } = expr
    {
        let is_obj_match = ident.sym == obj;
        let has_binding = ident.span.ctxt.outer() != unresolved_mark;
        let is_prop_match = prop_ident.sym == prop;
        is_obj_match && (check_obj_binding && !has_binding) && is_prop_match
    } else {
        false
    }
}
