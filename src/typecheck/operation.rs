//! Typing of primitive operations.
use super::*;
use crate::{
    error::TypecheckError,
    term::{BinaryOp, NAryOp, UnaryOp},
    types::AbsType,
};
use crate::{mk_tyw_arrow, mk_tyw_enum, mk_tyw_enum_row, mk_tyw_record, mk_tyw_row};

/// Type of unary operations.
pub fn get_uop_type(
    state: &mut State,
    op: &UnaryOp,
) -> Result<(TypeWrapper, TypeWrapper), TypecheckError> {
    Ok(match op {
        // forall a. bool -> a -> a -> a
        UnaryOp::Ite() => {
            let branches = TypeWrapper::Ptr(new_var(state.table));

            (
                mk_typewrapper::bool(),
                mk_tyw_arrow!(branches.clone(), branches.clone(), branches),
            )
        }
        // forall a. a -> Bool
        UnaryOp::IsNum()
        | UnaryOp::IsBool()
        | UnaryOp::IsStr()
        | UnaryOp::IsFun()
        | UnaryOp::IsList()
        | UnaryOp::IsRecord() => {
            let inp = TypeWrapper::Ptr(new_var(state.table));
            (inp, mk_typewrapper::bool())
        }
        // Bool -> Bool -> Bool
        UnaryOp::BoolAnd() | UnaryOp::BoolOr() => (
            mk_typewrapper::bool(),
            mk_tyw_arrow!(AbsType::Bool(), AbsType::Bool()),
        ),
        // Bool -> Bool
        UnaryOp::BoolNot() => (mk_typewrapper::bool(), mk_typewrapper::bool()),
        // forall a. Dyn -> a
        UnaryOp::Blame() => {
            let res = TypeWrapper::Ptr(new_var(state.table));

            (mk_typewrapper::dynamic(), res)
        }
        // Dyn -> Bool
        UnaryOp::Pol() => (mk_typewrapper::dynamic(), mk_typewrapper::bool()),
        // forall rows. < | rows> -> <id | rows>
        UnaryOp::Embed(id) => {
            let row = TypeWrapper::Ptr(new_var(state.table));
            // Constraining a freshly created variable should never fail.
            constraint(state, row.clone(), id.clone()).unwrap();
            (mk_tyw_enum!(row.clone()), mk_tyw_enum!(id.clone(), row))
        }
        // This should not happen, as Switch() is only produced during evaluation.
        UnaryOp::Switch(_) => panic!("cannot typecheck Switch()"),
        // Dyn -> Dyn
        UnaryOp::ChangePolarity() | UnaryOp::GoDom() | UnaryOp::GoCodom() | UnaryOp::GoList() => {
            (mk_typewrapper::dynamic(), mk_typewrapper::dynamic())
        }
        // Sym -> Dyn -> Dyn
        UnaryOp::Wrap() => (
            mk_typewrapper::sym(),
            mk_tyw_arrow!(AbsType::Dyn(), AbsType::Dyn()),
        ),
        // forall rows a. { id: a | rows} -> a
        UnaryOp::StaticAccess(id) => {
            let row = TypeWrapper::Ptr(new_var(state.table));
            let res = TypeWrapper::Ptr(new_var(state.table));

            (mk_tyw_record!((id.clone(), res.clone()); row), res)
        }
        // forall a b. List a -> (a -> b) -> List b
        UnaryOp::ListMap() => {
            let a = TypeWrapper::Ptr(new_var(state.table));
            let b = TypeWrapper::Ptr(new_var(state.table));

            let f_type = mk_tyw_arrow!(a.clone(), b.clone());
            (
                mk_typewrapper::list(a),
                mk_tyw_arrow!(f_type, mk_typewrapper::list(b)),
            )
        }
        // forall a. Num -> (Num -> a) -> List a
        UnaryOp::ListGen() => {
            let a = TypeWrapper::Ptr(new_var(state.table));

            let f_type = mk_tyw_arrow!(AbsType::Num(), a.clone());
            (
                mk_typewrapper::num(),
                mk_tyw_arrow!(f_type, mk_typewrapper::list(a)),
            )
        }
        // forall a b. { _ : a} -> (Str -> a -> b) -> { _ : b }
        UnaryOp::RecordMap() => {
            // Assuming f has type Str -> a -> b,
            // this has type DynRecord(a) -> DynRecord(b)

            let a = TypeWrapper::Ptr(new_var(state.table));
            let b = TypeWrapper::Ptr(new_var(state.table));

            let f_type = mk_tyw_arrow!(AbsType::Str(), a.clone(), b.clone());
            (
                mk_typewrapper::dyn_record(a),
                mk_tyw_arrow!(f_type, mk_typewrapper::dyn_record(b)),
            )
        }
        // forall a b. a -> b -> b
        UnaryOp::Seq() | UnaryOp::DeepSeq() => {
            let fst = TypeWrapper::Ptr(new_var(state.table));
            let snd = TypeWrapper::Ptr(new_var(state.table));

            (fst, mk_tyw_arrow!(snd.clone(), snd))
        }
        // forall a. List a -> a
        UnaryOp::ListHead() => {
            let ty_elt = TypeWrapper::Ptr(new_var(state.table));
            (mk_typewrapper::list(ty_elt.clone()), ty_elt)
        }
        // forall a. List a -> List a
        UnaryOp::ListTail() => {
            let ty_elt = TypeWrapper::Ptr(new_var(state.table));
            (
                mk_typewrapper::list(ty_elt.clone()),
                mk_typewrapper::list(ty_elt),
            )
        }
        // forall a. List a -> Num
        UnaryOp::ListLength() => {
            let ty_elt = TypeWrapper::Ptr(new_var(state.table));
            (mk_typewrapper::list(ty_elt), mk_typewrapper::num())
        }
        // This should not happen, as ChunksConcat() is only produced during evaluation.
        UnaryOp::ChunksConcat() => panic!("cannot type ChunksConcat()"),
        // BEFORE: forall rows. { rows } -> List
        // Dyn -> List Str
        UnaryOp::FieldsOf() => (
            mk_typewrapper::dynamic(),
            //mk_tyw_record!(; TypeWrapper::Ptr(new_var(state.table))),
            mk_typewrapper::list(AbsType::Str()),
        ),
        // Dyn -> List
        UnaryOp::ValuesOf() => (
            mk_typewrapper::dynamic(),
            mk_typewrapper::list(AbsType::Dyn()),
        ),
        // Str -> Str
        UnaryOp::StrTrim() => (mk_typewrapper::str(), mk_typewrapper::str()),
        // Str -> List Str
        UnaryOp::StrChars() => (
            mk_typewrapper::str(),
            mk_typewrapper::list(mk_typewrapper::str()),
        ),
        // Str -> Num
        UnaryOp::CharCode() => (mk_typewrapper::str(), mk_typewrapper::num()),
        // Num -> Str
        UnaryOp::CharFromCode() => (mk_typewrapper::num(), mk_typewrapper::str()),
        // Str -> Str
        UnaryOp::StrUppercase() => (mk_typewrapper::str(), mk_typewrapper::str()),
        // Str -> Str
        UnaryOp::StrLowercase() => (mk_typewrapper::str(), mk_typewrapper::str()),
        // Str -> Num
        UnaryOp::StrLength() => (mk_typewrapper::str(), mk_typewrapper::num()),
        // Dyn -> Str
        UnaryOp::ToStr() => (mk_typewrapper::dynamic(), mk_typewrapper::num()),
        // Str -> Num
        UnaryOp::NumFromStr() => (mk_typewrapper::str(), mk_typewrapper::num()),
        // Str -> < | Dyn>
        UnaryOp::EnumFromStr() => (
            mk_typewrapper::str(),
            mk_tyw_enum!(mk_typewrapper::dynamic()),
        ),
    })
}

/// Type of a binary operation.
pub fn get_bop_type(
    state: &mut State,
    op: &BinaryOp,
) -> Result<(TypeWrapper, TypeWrapper, TypeWrapper), TypecheckError> {
    Ok(match op {
        // Num -> Num -> Num
        BinaryOp::Plus()
        | BinaryOp::Sub()
        | BinaryOp::Mult()
        | BinaryOp::Div()
        | BinaryOp::Modulo() => (
            mk_typewrapper::num(),
            mk_typewrapper::num(),
            mk_typewrapper::num(),
        ),
        // Str -> Str -> Str
        BinaryOp::StrConcat() => (
            mk_typewrapper::str(),
            mk_typewrapper::str(),
            mk_typewrapper::str(),
        ),
        // Sym -> Dyn -> Dyn -> Dyn
        // This should not happen, as `ApplyContract()` is only produced during evaluation.
        BinaryOp::Assume() => panic!("cannot typecheck assume"),
        BinaryOp::Unwrap() => (
            mk_typewrapper::sym(),
            mk_typewrapper::dynamic(),
            mk_tyw_arrow!(AbsType::Dyn(), AbsType::Dyn()),
        ),
        // Str -> Dyn -> Dyn
        BinaryOp::Tag() => (
            mk_typewrapper::str(),
            mk_typewrapper::dynamic(),
            mk_typewrapper::dynamic(),
        ),
        // forall a b. a -> b -> Bool
        BinaryOp::Eq() => (
            TypeWrapper::Ptr(new_var(state.table)),
            TypeWrapper::Ptr(new_var(state.table)),
            mk_typewrapper::bool(),
        ),
        // Num -> Num -> Bool
        BinaryOp::LessThan()
        | BinaryOp::LessOrEq()
        | BinaryOp::GreaterThan()
        | BinaryOp::GreaterOrEq() => (
            mk_typewrapper::num(),
            mk_typewrapper::num(),
            mk_typewrapper::bool(),
        ),
        // Str -> Dyn -> Dyn
        BinaryOp::GoField() => (
            mk_typewrapper::str(),
            mk_typewrapper::dynamic(),
            mk_typewrapper::dynamic(),
        ),
        // forall a. Str -> { _ : a} -> a
        BinaryOp::DynAccess() => {
            let res = TypeWrapper::Ptr(new_var(state.table));

            (
                mk_typewrapper::str(),
                mk_typewrapper::dyn_record(res.clone()),
                res,
            )
        }
        // forall a. Str -> { _ : a } -> a -> { _ : a }
        BinaryOp::DynExtend() => {
            let res = TypeWrapper::Ptr(new_var(state.table));
            (
                mk_typewrapper::str(),
                mk_typewrapper::dyn_record(res.clone()),
                mk_tyw_arrow!(res.clone(), mk_typewrapper::dyn_record(res)),
            )
        }
        // forall a. Str -> { _ : a } -> { _ : a}
        BinaryOp::DynRemove() => {
            let res = TypeWrapper::Ptr(new_var(state.table));

            (
                mk_typewrapper::str(),
                mk_typewrapper::dyn_record(res.clone()),
                mk_typewrapper::dyn_record(res),
            )
        }
        // Str -> Dyn -> Bool
        BinaryOp::HasField() => (
            mk_typewrapper::str(),
            mk_typewrapper::dynamic(),
            mk_typewrapper::bool(),
        ),
        // forall a. List a -> List a -> List a
        BinaryOp::ListConcat() => {
            let ty_elt = TypeWrapper::Ptr(new_var(state.table));
            let ty_list = mk_typewrapper::list(ty_elt);
            (ty_list.clone(), ty_list.clone(), ty_list)
        }
        // forall a. List a -> Num -> a
        BinaryOp::ListElemAt() => {
            let ty_elt = TypeWrapper::Ptr(new_var(state.table));
            (
                mk_typewrapper::list(ty_elt.clone()),
                mk_typewrapper::num(),
                ty_elt,
            )
        }
        // Dyn -> Dyn -> Dyn
        BinaryOp::Merge() => (
            mk_typewrapper::dynamic(),
            mk_typewrapper::dynamic(),
            mk_typewrapper::dynamic(),
        ),
        // <Md5, Sha1, Sha256, Sha512> -> Str -> Str
        BinaryOp::Hash() => (
            mk_tyw_enum!(
                "Md5",
                "Sha1",
                "Sha256",
                "Sha512",
                mk_typewrapper::row_empty()
            ),
            mk_typewrapper::str(),
            mk_typewrapper::str(),
        ),
        // forall a. <Json, Yaml, Toml> -> a -> Str
        BinaryOp::Serialize() => {
            let ty_input = TypeWrapper::Ptr(new_var(state.table));
            (
                mk_tyw_enum!("Json", "Yaml", "Toml", mk_typewrapper::row_empty()),
                ty_input,
                mk_typewrapper::str(),
            )
        }
        // <Json, Yaml, Toml> -> Str -> Dyn
        BinaryOp::Deserialize() => (
            mk_tyw_enum!("Json", "Yaml", "Toml", mk_typewrapper::row_empty()),
            mk_typewrapper::str(),
            mk_typewrapper::dynamic(),
        ),
        // Num -> Num -> Num
        BinaryOp::Pow() => (
            mk_typewrapper::num(),
            mk_typewrapper::num(),
            mk_typewrapper::num(),
        ),
        // Str -> Str -> Bool
        BinaryOp::StrContains() => (
            mk_typewrapper::str(),
            mk_typewrapper::str(),
            mk_typewrapper::bool(),
        ),
        // Str -> Str -> Bool
        BinaryOp::StrIsMatch() => (
            mk_typewrapper::str(),
            mk_typewrapper::str(),
            mk_typewrapper::bool(),
        ),
        // Str -> Str -> {match: Str, index: Num, groups: List Str}
        BinaryOp::StrMatch() => (
            mk_typewrapper::str(),
            mk_typewrapper::str(),
            mk_tyw_record!(
                ("match", AbsType::Str()),
                ("index", AbsType::Num()),
                ("groups", mk_typewrapper::list(AbsType::Str()))
            ),
        ),
        // Str -> Str -> List Str
        BinaryOp::StrSplit() => (
            mk_typewrapper::str(),
            mk_typewrapper::str(),
            mk_typewrapper::list(AbsType::Str()),
        ),
    })
}

pub fn get_nop_type(
    _state: &mut State,
    op: &NAryOp,
) -> Result<(Vec<TypeWrapper>, TypeWrapper), TypecheckError> {
    Ok(match op {
        // Str -> Str -> Str -> Str
        NAryOp::StrReplace() | NAryOp::StrReplaceRegex() => (
            vec![
                mk_typewrapper::str(),
                mk_typewrapper::str(),
                mk_typewrapper::str(),
            ],
            mk_typewrapper::str(),
        ),
        // Str -> Num -> Num -> Str
        NAryOp::StrSubstr() => (
            vec![
                mk_typewrapper::str(),
                mk_typewrapper::num(),
                mk_typewrapper::num(),
            ],
            mk_typewrapper::str(),
        ),
        // This should not happen, as Switch() is only produced during evaluation.
        NAryOp::MergeContract() => panic!("cannot typecheck MergeContract()"),
    })
}
