use crate::ast::{
	CalleeKind, Class, Expr, ExprKind, FunctionBody, FunctionDefinition, Phase, Reference, Scope, Stmt, StmtKind, Symbol,
	TypeAnnotation, TypeAnnotationKind,
};
use crate::diagnostic::WingSpan;
use crate::docs::Documented;
use crate::lsp::sync::FILES;
use crate::type_check::symbol_env::LookupResult;
use crate::type_check::{resolve_super_method, ClassLike, Type, Types, CLASS_INFLIGHT_INIT_NAME, CLASS_INIT_NAME};
use crate::visit::{self, Visit};
use crate::wasm_util::WASM_RETURN_ERROR;
use crate::wasm_util::{ptr_to_string, string_to_combined_ptr};
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

pub struct HoverVisitor<'a> {
	position: Position,
	types: &'a Types,
	current_scope: &'a Scope,
	current_expr: Option<&'a Expr>,
	current_statement_index: usize,
	found: Option<(WingSpan, Option<String>)>,
}

impl<'a> HoverVisitor<'a> {
	pub fn new(position: Position, scope: &'a Scope, types: &'a Types) -> Self {
		Self {
			types,
			position,
			current_scope: scope,
			current_expr: None,
			current_statement_index: 0,
			found: None,
		}
	}

	pub fn visit(&mut self) -> Option<(WingSpan, Option<String>)> {
		self.visit_scope(self.current_scope);
		self.found.clone()
	}

	/// Try to look up a full path of a symbol in the current scope and if found, render the docs
	/// associated with the symbol kind. Returns `None` if not found.
	fn lookup_docs(&mut self, nested_str: &str, property: Option<&Symbol>) -> Option<String> {
		let current_env = self.current_scope.env.borrow();
		let current_env = current_env.as_ref()?;

		let result = current_env.lookup_nested_str(nested_str, None);

		if let LookupResult::Found(s, _) = result {
			// if `property` is also specified, then continue to lookup within the found type
			if let Some(p) = property {
				if let Some(b) = s.as_type() {
					if let Some(x) = b.as_class() {
						if let Some(k) = x.env.lookup(&p, None) {
							return Some(k.render_docs());
						}
					}
				}
			}

			return Some(s.render_docs());
		}

		None
	}

	fn with_scope(&mut self, scope: &'a Scope, mut f: impl FnMut(&mut Self)) {
		let last_scope = self.current_scope;
		self.current_scope = scope;
		f(self);
		if self.found.is_none() {
			self.current_scope = last_scope;
		}
	}

	fn visit_reference_with_member(&mut self, object: &'a Expr, property: &'a Symbol) {
		if object.span.contains(&self.position) {
			self.visit_expr(object);
			return;
		}
		let obj_type = self.types.get_expr_type(object);
		if property.span.contains(&self.position) {
			let new_span = self.current_expr.unwrap().span.clone();
			match &**obj_type.maybe_unwrap_option() {
				Type::Optional(_) | Type::Anything | Type::Void | Type::Nil | Type::Unresolved => {}

				Type::Array(_)
				| Type::MutArray(_)
				| Type::Map(_)
				| Type::MutMap(_)
				| Type::Set(_)
				| Type::MutSet(_)
				| Type::Json
				| Type::MutJson
				| Type::Number
				| Type::String
				| Type::Duration
				| Type::Boolean => {
					if let Some((std_type, ..)) = self.types.get_std_class(&obj_type.to_string()) {
						if let Some(c) = std_type.as_type() {
							if let Some(c) = c.as_class() {
								self.found = Some((new_span, docs_from_classlike_property(c, property)));
							}
						}
					}
				}

				Type::Function(_) | Type::Enum(_) => {
					self.found = Some((
						new_span,
						Some(self.types.get_expr_type(self.current_expr.unwrap()).render_docs()),
					));
				}
				Type::Class(c) => {
					self.found = Some((new_span, docs_from_classlike_property(c, property)));
				}
				Type::Interface(c) => {
					self.found = Some((new_span, docs_from_classlike_property(c, property)));
				}
				Type::Struct(c) => {
					self.found = Some((new_span, docs_from_classlike_property(c, property)));
				}
			}
		}
	}
}

impl<'a> Visit<'a> for HoverVisitor<'a> {
	fn visit_stmt(&mut self, node: &'a Stmt) {
		if self.found.is_some() {
			return;
		}
		self.current_statement_index = node.idx;

		// Handle situations where symbols are actually defined in inner scopes
		match &node.kind {
			StmtKind::ForLoop {
				iterator,
				iterable,
				statements,
			} => {
				self.with_scope(statements, |v| {
					v.visit_symbol(iterator);
				});
				self.visit_expr(iterable);
				self.visit_scope(statements);
			}
			StmtKind::TryCatch {
				try_statements,
				catch_block,
				finally_statements,
			} => {
				self.visit_scope(try_statements);
				if let Some(catch_block) = catch_block {
					if let Some(exception_var) = &catch_block.exception_var {
						self.with_scope(&catch_block.statements, |v| {
							v.visit_symbol(exception_var);
						});
					}
					self.visit_scope(&catch_block.statements);
				}
				if let Some(finally_statements) = finally_statements {
					self.visit_scope(finally_statements);
				}
			}
			_ => crate::visit::visit_stmt(self, node),
		}
	}

	fn visit_scope(&mut self, node: &'a Scope) {
		if self.found.is_some() {
			return;
		}

		self.with_scope(node, |this| {
			for stmt in &node.statements {
				this.visit_stmt(stmt);
			}
		});
	}

	fn visit_type_annotation(&mut self, node: &'a TypeAnnotation) {
		if self.found.is_some() {
			return;
		}

		if let TypeAnnotationKind::UserDefined(t) = &node.kind {
			if t.span.contains(&self.position) {
				self.found = Some((t.span.clone(), self.lookup_docs(&t.full_path_str(), None)));
			}
		}

		visit::visit_type_annotation(self, node);
	}

	fn visit_symbol(&mut self, node: &'a Symbol) {
		if self.found.is_some() {
			return;
		}

		if node.span.contains(&self.position) {
			self.found = Some((node.span.clone(), self.lookup_docs(&node.name, None)));
		}

		visit::visit_symbol(self, node);
	}

	fn visit_expr(&mut self, node: &'a Expr) {
		if self.found.is_some() {
			return;
		}

		let last_expr = self.current_expr;
		self.current_expr = Some(node);

		match &node.kind {
			ExprKind::New(new_expr) => {
				let x = new_expr
					.arg_list
					.named_args
					.iter()
					.find(|a| a.0.span.contains(&self.position));
				if let Some((arg_name, ..)) = x {
					// we need to get the struct type from the class constructor
					let class_type = self.types.get_expr_type(node);
					let class_phase = self.types.get_expr_phase(node).unwrap();
					let class_type = class_type.as_class().unwrap();
					let init_info = match class_phase {
						Phase::Inflight => class_type.get_method(&Symbol::global(CLASS_INFLIGHT_INIT_NAME)),
						Phase::Preflight => class_type.get_method(&Symbol::global(CLASS_INIT_NAME)),
						Phase::Independent => panic!("Cannot get hover info for independent class"),
					};
					if let Some(var_info) = init_info {
						if let Some(structy) = var_info.type_.get_function_struct_arg() {
							self.found = Some((arg_name.span.clone(), docs_from_classlike_property(structy, arg_name)));
						}
					}
				}
			}
			ExprKind::Call { arg_list, callee } => {
				let x = arg_list.named_args.iter().find(|a| a.0.span.contains(&self.position));
				if let Some((arg_name, ..)) = x {
					let curr_env = self.current_scope.env.borrow();
					let env = curr_env.as_ref().expect("an env");
					// we need to get the struct type from the callee
					let callee_type = match callee {
						CalleeKind::Expr(expr) => self.types.get_expr_type(expr),
						CalleeKind::SuperCall(method) => resolve_super_method(method, env, &self.types)
							.ok()
							.map_or(self.types.error(), |t| t.0),
					};

					if let Some(structy) = callee_type.get_function_struct_arg() {
						self.found = Some((arg_name.span.clone(), docs_from_classlike_property(structy, arg_name)));
					}
				}
			}
			ExprKind::StructLiteral { fields, .. } => {
				if let Some(f) = fields.iter().find(|f| f.0.span.contains(&self.position)) {
					let field_name = f.0;
					let struct_type = self.types.get_expr_type(node);
					if let Some(structy) = struct_type.maybe_unwrap_option().as_struct() {
						self.found = Some((
							field_name.span.clone(),
							docs_from_classlike_property(structy, field_name),
						));
					}
					return;
				}
			}
			_ => {}
		}

		crate::visit::visit_expr(self, node);

		if self.found.is_none() {
			self.current_expr = last_expr;
		}
	}

	fn visit_class(&mut self, node: &'a Class) {
		if self.found.is_some() {
			return;
		}
		self.visit_symbol(&node.name);

		self.visit_function_definition(&node.initializer);

		let scope = if let FunctionBody::Statements(statements) = &node.initializer.body {
			statements
		} else {
			panic!("Initializer cannot be 'extern'");
		};

		self.with_scope(&scope, |v| {
			for field in &node.fields {
				if field.name.span.contains(&v.position) {
					v.found = Some((
						field.name.span.clone(),
						v.lookup_docs(&node.name.name, Some(&field.name)),
					));
				}

				v.visit_type_annotation(&field.member_type);
			}

			for method in &node.methods {
				if method.0.span.contains(&v.position) {
					v.found = Some((method.0.span.clone(), v.lookup_docs(&node.name.name, Some(&method.0))));
				}
				v.visit_function_definition(&method.1);
			}
		});
	}

	fn visit_function_definition(&mut self, node: &'a FunctionDefinition) {
		if self.found.is_some() {
			return;
		}

		if let FunctionBody::Statements(scope) = &node.body {
			self.with_scope(scope, |v| {
				for param in &node.signature.parameters {
					v.visit_function_parameter(&param);
				}
			});
		}

		self.visit_type_annotation(&node.signature.return_type);

		if let FunctionBody::Statements(scope) = &node.body {
			self.visit_scope(scope);
		}
	}

	fn visit_reference(&mut self, node: &'a Reference) {
		if self.found.is_some() {
			return;
		}

		match node {
			Reference::Identifier(sym) => {
				if sym.span.contains(&self.position) {
					self.found = Some((sym.span.clone(), self.lookup_docs(&sym.name, None)));
				}
			}
			Reference::TypeReference(t) => {
				if t.span.contains(&self.position) {
					// Only lookup string up to the position
					let mut partial_path = vec![];
					t.full_path().iter().for_each(|p| {
						if p.span.start <= self.position.into() {
							partial_path.push(p.name.clone());
						}
					});
					let lookup_str = partial_path.join(".");
					self.found = Some((t.span.clone(), self.lookup_docs(&lookup_str, None)));
				}
			}
			Reference::InstanceMember { object, property, .. } => self.visit_reference_with_member(object, property),
			Reference::TypeMember { typeobject, property } => self.visit_reference_with_member(&typeobject, property),
		}

		visit::visit_reference(self, node);
	}
}

#[no_mangle]
pub unsafe extern "C" fn wingc_on_hover(ptr: u32, len: u32) -> u64 {
	let parse_string = ptr_to_string(ptr, len);
	if let Ok(parsed) = serde_json::from_str(&parse_string) {
		if let Some(token_result) = on_hover(parsed) {
			let result = serde_json::to_string(&token_result).expect("Failed to serialize Hover response");

			string_to_combined_ptr(result)
		} else {
			WASM_RETURN_ERROR
		}
	} else {
		eprintln!("Failed to parse 'onHover' text document: {}", parse_string);
		WASM_RETURN_ERROR
	}
}
pub fn on_hover(params: lsp_types::HoverParams) -> Option<Hover> {
	FILES.with(|files| {
		let files = files.borrow();
		let file_data = files.get(&params.text_document_position_params.text_document.uri.clone());
		let file_data = file_data.expect(
			format!(
				"Compiled data not found for \"{}\"",
				params.text_document_position_params.text_document.uri
			)
			.as_str(),
		);

		let root_scope = &file_data.scope;

		let mut hover_visitor = HoverVisitor::new(
			params.text_document_position_params.position,
			&root_scope,
			&file_data.types,
		);
		if let Some((span, Some(docs))) = hover_visitor.visit() {
			Some(Hover {
				contents: HoverContents::Markup(MarkupContent {
					kind: MarkupKind::Markdown,
					value: docs,
				}),
				range: Some(span.clone().into()),
			})
		} else {
			None
		}
	})
}

fn docs_from_classlike_property(classlike: &impl ClassLike, property: &Symbol) -> Option<String> {
	let property = classlike.get_env().lookup(property, None)?;
	Some(property.render_docs())
}

#[cfg(test)]
mod tests {
	use crate::lsp::hover::*;
	use crate::lsp::sync::test_utils::*;
	use lsp_types::*;

	/// Creates a snapshot test for a given wing program's hover at a given position
	/// In the wing program, place a comment "//^" into the text where the "^" is pointing to the desired character position
	///
	/// First parameter will be the name of the tests, as well as the identifier to use for the list of completion in the asserts (see last parameter)
	/// Second parameter is the wing code block as a string literal
	/// After the first two parameters, any additional are optional statements that should be used for asserting on the given hover data.
	///
	/// Result is a [Hover] object
	macro_rules! test_hover_list {
		($name:ident, $code:literal) => {
			test_hover_list!($name, $code,);
		};
		($name:ident, $code:literal, $($assertion:stmt)*) => {
			#[test]
			fn $name() {
				// NOTE: this is needed for debugging to work regardless of where you run the test
				std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).unwrap();

				let text_document_position_params = load_file_with_contents($code);
				let hover = on_hover(HoverParams {
					text_document_position_params,
					work_done_progress_params: Default::default(),
				});

				if let Some($name) = hover {
					insta::with_settings!(
						{
							prepend_module_to_snapshot => false,
							omit_expression => true,
							snapshot_path => "./snapshots/hovers",
						}, {
							insta::assert_yaml_snapshot!($name);
						}
					);
					$($assertion)*
				} else {
					panic!("Expected hover data");
				}
			}
		};
	}

	test_hover_list!(
		just_variable,
		r#"
		let myString = "hello";
        //^
		"#,
	);

	test_hover_list!(
		new_expression_nested,
		r#"
bring cloud;
new cloud. 
		//^
"#,
	);

	test_hover_list!(
		class_symbol,
		r#"
		bring cloud;

		let bucket = new cloud.Bucket();
        //^
"#,
	);

	test_hover_list!(
		class_symbol_in_closure,
		r#"
inflight class MyClass { }

inflight () => {
  let myClass = new MyClass();
    //^

}
"#,
	);

	test_hover_list!(
		class_property,
		r#"
bring cloud;

let bucket = new cloud.Bucket();
bucket.addObject
      //^
"#,
	);

	test_hover_list!(
		static_stdtype_method,
		r#"
Json.stringify(123);
      //^
"#,
	);

	test_hover_list!(
		inside_inflight_test,
		r#"
test "hello" {
  let a = 1;
    //^
}
"#,
	);

	test_hover_list!(
		inside_class_method,
		r#"
class Foo {
  bar() {
    let hello = 1;
        //^
  }
}
"#,
	);

	test_hover_list!(
		inside_class_field,
		r#"
bring cloud;
class Foo {
  my_bucket: cloud.Bucket;
    //^
}
"#,
	);

	test_hover_list!(
		new_statement,
		r#"
bring cloud;
new cloud.Bucket();
          //^
"#
	);

	test_hover_list!(
		user_defined_types,
		r#"
class Foo { };
     //^
"#
	);

	test_hover_list!(
		static_method,
		r#"
class Foo {
  static my(a: str, b: bool): str { return "str"; }
}

Foo.my();
  //^
"#
	);

	test_hover_list!(
		builtin_in_preflight,
		r#"
assert(true);
//^
"#
	);

	test_hover_list!(
		builtin_in_inflight,
		r#"
class Foo {
  inflight bar() {
    throw("hello");
    //^
  }
}
"#
	);

	test_hover_list!(
		test_statement,
		r#"
test "foo" {
//^
};
"#
	);

	test_hover_list!(
		test_bring_sdk,
		r#"
bring cloud;
      //^
"#
	);

	test_hover_list!(
		test_bring_library,
		r#"
bring "@winglang/sdk" as bar;
                        //^
"#
	);

	test_hover_list!(
		test_var,
		r#"
let var xoo = "hello";
log(xoo);
    //^
"#
	);

	test_hover_list!(
		test_var_inside_preflight_closure,
		r#"
() => {
  let var goooo = "gar";
          //^
}"#
	);

	test_hover_list!(
		test_var_inside_inflight_closure,
		r#"
inflight () => {
  let var goooo = "gar";
          //^
}"#
	);

	test_hover_list!(
		test_builtin_instance_method,
		r#"
"hello".startsWith("h");
           //^
"#
	);

	test_hover_list!(
		multipart_reference_hover_middle,
		r#"
let j = Json {};
j.get("hello").get("world");
 //^
"#
	);
}
