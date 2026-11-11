(set-option :auto_config false)
(set-option :smt.mbqi false)
(set-option :smt.case_split 3)
(set-option :smt.qi.eager_threshold 100.0)
(set-option :smt.delay_units true)
(set-option :smt.arith.solver 2)
(set-option :smt.arith.nl false)
(set-option :pi.enabled false)
(set-option :rewriter.sort_disjunctions false)
(declare-sort %%Function%% 0)
(declare-sort FuelId 0)
(declare-sort Fuel 0)
(declare-const zero Fuel)
(declare-fun succ (Fuel) Fuel)
(declare-fun fuel_bool (FuelId) Bool)
(declare-fun fuel_bool_default (FuelId) Bool)
(declare-const fuel_defaults Bool)
(assert
 (=>
  fuel_defaults
  (forall ((id FuelId)) (!
    (= (fuel_bool id) (fuel_bool_default id))
    :pattern ((fuel_bool id))
    :qid prelude_fuel_defaults
    :skolemid skolem_prelude_fuel_defaults
))))
(declare-datatypes ((fndef 0)) (((fndef_singleton))))
(declare-sort Poly 0)
(declare-sort Height 0)
(declare-fun I (Int) Poly)
(declare-fun B (Bool) Poly)
(declare-fun R (Real) Poly)
(declare-fun F (fndef) Poly)
(declare-fun %I (Poly) Int)
(declare-fun %B (Poly) Bool)
(declare-fun %R (Poly) Real)
(declare-fun %F (Poly) fndef)
(declare-sort Type 0)
(declare-const BOOL Type)
(declare-const INT Type)
(declare-const NAT Type)
(declare-const REAL Type)
(declare-const CHAR Type)
(declare-const USIZE Type)
(declare-const ISIZE Type)
(declare-const TYPE%tuple%0. Type)
(declare-fun UINT (Int) Type)
(declare-fun SINT (Int) Type)
(declare-fun FLOAT (Int) Type)
(declare-fun CONST_INT (Int) Type)
(declare-fun CONST_BOOL (Bool) Type)
(declare-sort Dcr 0)
(declare-const $ Dcr)
(declare-const $slice Dcr)
(declare-const $dyn Dcr)
(declare-fun DST (Dcr) Dcr)
(declare-fun REF (Dcr) Dcr)
(declare-fun BOX (Dcr Type Dcr) Dcr)
(declare-fun RC (Dcr Type Dcr) Dcr)
(declare-fun ARC (Dcr Type Dcr) Dcr)
(declare-fun GHOST (Dcr) Dcr)
(declare-fun TRACKED (Dcr) Dcr)
(declare-fun NEVER (Dcr) Dcr)
(declare-fun CONST_PTR (Dcr) Dcr)
(declare-fun ARRAY (Dcr Type Dcr Type) Type)
(declare-fun MUTREF (Dcr Type) Type)
(declare-fun SLICE (Dcr Type) Type)
(declare-const STRSLICE Type)
(declare-const ALLOCATOR_GLOBAL Type)
(declare-fun PTR (Dcr Type) Type)
(declare-fun has_type (Poly Type) Bool)
(declare-fun sized (Dcr) Bool)
(declare-fun as_type (Poly Type) Poly)
(declare-fun mk_fun (Type %%Function%%) %%Function%%)
(declare-fun const_int (Type) Int)
(declare-fun const_bool (Type) Bool)
(declare-fun mut_ref_current% (Poly) Poly)
(declare-fun mut_ref_future% (Poly) Poly)
(declare-fun mut_ref_update_current% (Poly Poly) Poly)
(assert
 (forall ((m Poly) (arg Poly)) (!
   (= (mut_ref_current% (mut_ref_update_current% m arg)) arg)
   :pattern ((mut_ref_update_current% m arg))
   :qid prelude_mut_ref_update_current_current
   :skolemid skolem_prelude_mut_ref_update_current_current
)))
(assert
 (forall ((m Poly) (arg Poly)) (!
   (= (mut_ref_future% (mut_ref_update_current% m arg)) (mut_ref_future% m))
   :pattern ((mut_ref_update_current% m arg))
   :qid prelude_mut_ref_update_current_future
   :skolemid skolem_prelude_mut_ref_update_current_future
)))
(assert
 (forall ((m Poly) (d Dcr) (t Type)) (!
   (=>
    (has_type m (MUTREF d t))
    (has_type (mut_ref_current% m) t)
   )
   :pattern ((has_type m (MUTREF d t)) (mut_ref_current% m))
   :qid prelude_mut_ref_current_has_type
   :skolemid skolem_prelude_mut_ref_current_has_type
)))
(assert
 (forall ((m Poly) (d Dcr) (t Type)) (!
   (=>
    (has_type m (MUTREF d t))
    (has_type (mut_ref_future% m) t)
   )
   :pattern ((has_type m (MUTREF d t)) (mut_ref_future% m))
   :qid prelude_mut_ref_future_has_type
   :skolemid skolem_prelude_mut_ref_future_has_type
)))
(assert
 (forall ((m Poly) (d Dcr) (t Type) (arg Poly)) (!
   (=>
    (and
     (has_type m (MUTREF d t))
     (has_type arg t)
    )
    (has_type (mut_ref_update_current% m arg) (MUTREF d t))
   )
   :pattern ((has_type m (MUTREF d t)) (mut_ref_update_current% m arg))
   :qid prelude_mut_ref_update_has_type
   :skolemid skolem_prelude_mut_ref_update_has_type
)))
(assert
 (forall ((d Dcr)) (!
   (=>
    (sized d)
    (sized (DST d))
   )
   :pattern ((sized (DST d)))
   :qid prelude_sized_decorate_struct_inherit
   :skolemid skolem_prelude_sized_decorate_struct_inherit
)))
(assert
 (forall ((d Dcr)) (!
   (sized (REF d))
   :pattern ((sized (REF d)))
   :qid prelude_sized_decorate_ref
   :skolemid skolem_prelude_sized_decorate_ref
)))
(assert
 (forall ((d Dcr) (t Type) (d2 Dcr)) (!
   (sized (BOX d t d2))
   :pattern ((sized (BOX d t d2)))
   :qid prelude_sized_decorate_box
   :skolemid skolem_prelude_sized_decorate_box
)))
(assert
 (forall ((d Dcr) (t Type) (d2 Dcr)) (!
   (sized (RC d t d2))
   :pattern ((sized (RC d t d2)))
   :qid prelude_sized_decorate_rc
   :skolemid skolem_prelude_sized_decorate_rc
)))
(assert
 (forall ((d Dcr) (t Type) (d2 Dcr)) (!
   (sized (ARC d t d2))
   :pattern ((sized (ARC d t d2)))
   :qid prelude_sized_decorate_arc
   :skolemid skolem_prelude_sized_decorate_arc
)))
(assert
 (forall ((d Dcr)) (!
   (sized (GHOST d))
   :pattern ((sized (GHOST d)))
   :qid prelude_sized_decorate_ghost
   :skolemid skolem_prelude_sized_decorate_ghost
)))
(assert
 (forall ((d Dcr)) (!
   (sized (TRACKED d))
   :pattern ((sized (TRACKED d)))
   :qid prelude_sized_decorate_tracked
   :skolemid skolem_prelude_sized_decorate_tracked
)))
(assert
 (forall ((d Dcr)) (!
   (sized (NEVER d))
   :pattern ((sized (NEVER d)))
   :qid prelude_sized_decorate_never
   :skolemid skolem_prelude_sized_decorate_never
)))
(assert
 (forall ((d Dcr)) (!
   (sized (CONST_PTR d))
   :pattern ((sized (CONST_PTR d)))
   :qid prelude_sized_decorate_const_ptr
   :skolemid skolem_prelude_sized_decorate_const_ptr
)))
(assert
 (sized $)
)
(assert
 (forall ((i Int)) (!
   (= i (const_int (CONST_INT i)))
   :pattern ((CONST_INT i))
   :qid prelude_type_id_const_int
   :skolemid skolem_prelude_type_id_const_int
)))
(assert
 (forall ((b Bool)) (!
   (= b (const_bool (CONST_BOOL b)))
   :pattern ((CONST_BOOL b))
   :qid prelude_type_id_const_bool
   :skolemid skolem_prelude_type_id_const_bool
)))
(assert
 (forall ((b Bool)) (!
   (has_type (B b) BOOL)
   :pattern ((has_type (B b) BOOL))
   :qid prelude_has_type_bool
   :skolemid skolem_prelude_has_type_bool
)))
(assert
 (forall ((r Real)) (!
   (has_type (R r) REAL)
   :pattern ((has_type (R r) REAL))
   :qid prelude_has_type_real
   :skolemid skolem_prelude_has_type_real
)))
(assert
 (forall ((x Poly) (t Type)) (!
   (and
    (has_type (as_type x t) t)
    (=>
     (has_type x t)
     (= x (as_type x t))
   ))
   :pattern ((as_type x t))
   :qid prelude_as_type
   :skolemid skolem_prelude_as_type
)))
(assert
 (forall ((x Bool)) (!
   (= x (%B (B x)))
   :pattern ((B x))
   :qid prelude_unbox_box_bool
   :skolemid skolem_prelude_unbox_box_bool
)))
(assert
 (forall ((x Int)) (!
   (= x (%I (I x)))
   :pattern ((I x))
   :qid prelude_unbox_box_int
   :skolemid skolem_prelude_unbox_box_int
)))
(assert
 (forall ((x Real)) (!
   (= x (%R (R x)))
   :pattern ((R x))
   :qid prelude_unbox_box_real
   :skolemid skolem_prelude_unbox_box_real
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x BOOL)
    (= x (B (%B x)))
   )
   :pattern ((has_type x BOOL))
   :qid prelude_box_unbox_bool
   :skolemid skolem_prelude_box_unbox_bool
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x INT)
    (= x (I (%I x)))
   )
   :pattern ((has_type x INT))
   :qid prelude_box_unbox_int
   :skolemid skolem_prelude_box_unbox_int
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x NAT)
    (= x (I (%I x)))
   )
   :pattern ((has_type x NAT))
   :qid prelude_box_unbox_nat
   :skolemid skolem_prelude_box_unbox_nat
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x USIZE)
    (= x (I (%I x)))
   )
   :pattern ((has_type x USIZE))
   :qid prelude_box_unbox_usize
   :skolemid skolem_prelude_box_unbox_usize
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x ISIZE)
    (= x (I (%I x)))
   )
   :pattern ((has_type x ISIZE))
   :qid prelude_box_unbox_isize
   :skolemid skolem_prelude_box_unbox_isize
)))
(assert
 (forall ((bits Int) (x Poly)) (!
   (=>
    (has_type x (UINT bits))
    (= x (I (%I x)))
   )
   :pattern ((has_type x (UINT bits)))
   :qid prelude_box_unbox_uint
   :skolemid skolem_prelude_box_unbox_uint
)))
(assert
 (forall ((bits Int) (x Poly)) (!
   (=>
    (has_type x (SINT bits))
    (= x (I (%I x)))
   )
   :pattern ((has_type x (SINT bits)))
   :qid prelude_box_unbox_sint
   :skolemid skolem_prelude_box_unbox_sint
)))
(assert
 (forall ((bits Int) (x Poly)) (!
   (=>
    (has_type x (FLOAT bits))
    (= x (I (%I x)))
   )
   :pattern ((has_type x (FLOAT bits)))
   :qid prelude_box_unbox_float
   :skolemid skolem_prelude_box_unbox_float
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x CHAR)
    (= x (I (%I x)))
   )
   :pattern ((has_type x CHAR))
   :qid prelude_box_unbox_char
   :skolemid skolem_prelude_box_unbox_char
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x REAL)
    (= x (R (%R x)))
   )
   :pattern ((has_type x REAL))
   :qid prelude_box_unbox_real
   :skolemid skolem_prelude_box_unbox_real
)))
(declare-fun ext_eq (Bool Type Poly Poly) Bool)
(assert
 (forall ((deep Bool) (t Type) (x Poly) (y Poly)) (!
   (= (= x y) (ext_eq deep t x y))
   :pattern ((ext_eq deep t x y))
   :qid prelude_ext_eq
   :skolemid skolem_prelude_ext_eq
)))
(declare-const SZ Int)
(assert
 (or
  (= SZ 32)
  (= SZ 64)
))
(declare-fun uHi (Int) Int)
(declare-fun iLo (Int) Int)
(declare-fun iHi (Int) Int)
(assert
 (= (uHi 8) 256)
)
(assert
 (= (uHi 16) 65536)
)
(assert
 (= (uHi 32) 4294967296)
)
(assert
 (= (uHi 64) 18446744073709551616)
)
(assert
 (= (uHi 128) (+ 1 340282366920938463463374607431768211455))
)
(assert
 (= (iLo 8) (- 128))
)
(assert
 (= (iLo 16) (- 32768))
)
(assert
 (= (iLo 32) (- 2147483648))
)
(assert
 (= (iLo 64) (- 9223372036854775808))
)
(assert
 (= (iLo 128) (- 170141183460469231731687303715884105728))
)
(assert
 (= (iHi 8) 128)
)
(assert
 (= (iHi 16) 32768)
)
(assert
 (= (iHi 32) 2147483648)
)
(assert
 (= (iHi 64) 9223372036854775808)
)
(assert
 (= (iHi 128) 170141183460469231731687303715884105728)
)
(declare-fun nClip (Int) Int)
(declare-fun uClip (Int Int) Int)
(declare-fun iClip (Int Int) Int)
(declare-fun charClip (Int) Int)
(assert
 (forall ((i Int)) (!
   (and
    (<= 0 (nClip i))
    (=>
     (<= 0 i)
     (= i (nClip i))
   ))
   :pattern ((nClip i))
   :qid prelude_nat_clip
   :skolemid skolem_prelude_nat_clip
)))
(assert
 (forall ((bits Int) (i Int)) (!
   (and
    (<= 0 (uClip bits i))
    (< (uClip bits i) (uHi bits))
    (=>
     (and
      (<= 0 i)
      (< i (uHi bits))
     )
     (= i (uClip bits i))
   ))
   :pattern ((uClip bits i))
   :qid prelude_u_clip
   :skolemid skolem_prelude_u_clip
)))
(assert
 (forall ((bits Int) (i Int)) (!
   (and
    (<= (iLo bits) (iClip bits i))
    (< (iClip bits i) (iHi bits))
    (=>
     (and
      (<= (iLo bits) i)
      (< i (iHi bits))
     )
     (= i (iClip bits i))
   ))
   :pattern ((iClip bits i))
   :qid prelude_i_clip
   :skolemid skolem_prelude_i_clip
)))
(assert
 (forall ((i Int)) (!
   (and
    (or
     (and
      (<= 0 (charClip i))
      (<= (charClip i) 55295)
     )
     (and
      (<= 57344 (charClip i))
      (<= (charClip i) 1114111)
    ))
    (=>
     (or
      (and
       (<= 0 i)
       (<= i 55295)
      )
      (and
       (<= 57344 i)
       (<= i 1114111)
     ))
     (= i (charClip i))
   ))
   :pattern ((charClip i))
   :qid prelude_char_clip
   :skolemid skolem_prelude_char_clip
)))
(declare-fun uInv (Int Int) Bool)
(declare-fun iInv (Int Int) Bool)
(declare-fun charInv (Int) Bool)
(assert
 (forall ((bits Int) (i Int)) (!
   (= (uInv bits i) (and
     (<= 0 i)
     (< i (uHi bits))
   ))
   :pattern ((uInv bits i))
   :qid prelude_u_inv
   :skolemid skolem_prelude_u_inv
)))
(assert
 (forall ((bits Int) (i Int)) (!
   (= (iInv bits i) (and
     (<= (iLo bits) i)
     (< i (iHi bits))
   ))
   :pattern ((iInv bits i))
   :qid prelude_i_inv
   :skolemid skolem_prelude_i_inv
)))
(assert
 (forall ((i Int)) (!
   (= (charInv i) (or
     (and
      (<= 0 i)
      (<= i 55295)
     )
     (and
      (<= 57344 i)
      (<= i 1114111)
   )))
   :pattern ((charInv i))
   :qid prelude_char_inv
   :skolemid skolem_prelude_char_inv
)))
(assert
 (forall ((x Int)) (!
   (has_type (I x) INT)
   :pattern ((has_type (I x) INT))
   :qid prelude_has_type_int
   :skolemid skolem_prelude_has_type_int
)))
(assert
 (forall ((x Int)) (!
   (=>
    (<= 0 x)
    (has_type (I x) NAT)
   )
   :pattern ((has_type (I x) NAT))
   :qid prelude_has_type_nat
   :skolemid skolem_prelude_has_type_nat
)))
(assert
 (forall ((x Int)) (!
   (=>
    (uInv SZ x)
    (has_type (I x) USIZE)
   )
   :pattern ((has_type (I x) USIZE))
   :qid prelude_has_type_usize
   :skolemid skolem_prelude_has_type_usize
)))
(assert
 (forall ((x Int)) (!
   (=>
    (iInv SZ x)
    (has_type (I x) ISIZE)
   )
   :pattern ((has_type (I x) ISIZE))
   :qid prelude_has_type_isize
   :skolemid skolem_prelude_has_type_isize
)))
(assert
 (forall ((bits Int) (x Int)) (!
   (=>
    (uInv bits x)
    (has_type (I x) (UINT bits))
   )
   :pattern ((has_type (I x) (UINT bits)))
   :qid prelude_has_type_uint
   :skolemid skolem_prelude_has_type_uint
)))
(assert
 (forall ((bits Int) (x Int)) (!
   (=>
    (iInv bits x)
    (has_type (I x) (SINT bits))
   )
   :pattern ((has_type (I x) (SINT bits)))
   :qid prelude_has_type_sint
   :skolemid skolem_prelude_has_type_sint
)))
(assert
 (forall ((bits Int) (x Int)) (!
   (=>
    (uInv bits x)
    (has_type (I x) (FLOAT bits))
   )
   :pattern ((has_type (I x) (FLOAT bits)))
   :qid prelude_has_type_float
   :skolemid skolem_prelude_has_type_float
)))
(assert
 (forall ((x Int)) (!
   (=>
    (charInv x)
    (has_type (I x) CHAR)
   )
   :pattern ((has_type (I x) CHAR))
   :qid prelude_has_type_char
   :skolemid skolem_prelude_has_type_char
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x NAT)
    (<= 0 (%I x))
   )
   :pattern ((has_type x NAT))
   :qid prelude_unbox_int
   :skolemid skolem_prelude_unbox_int
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x USIZE)
    (uInv SZ (%I x))
   )
   :pattern ((has_type x USIZE))
   :qid prelude_unbox_usize
   :skolemid skolem_prelude_unbox_usize
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x CHAR)
    (charInv (%I x))
   )
   :pattern ((has_type x CHAR))
   :qid prelude_unbox_char
   :skolemid skolem_prelude_unbox_char
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x ISIZE)
    (iInv SZ (%I x))
   )
   :pattern ((has_type x ISIZE))
   :qid prelude_unbox_isize
   :skolemid skolem_prelude_unbox_isize
)))
(assert
 (forall ((bits Int) (x Poly)) (!
   (=>
    (has_type x (UINT bits))
    (uInv bits (%I x))
   )
   :pattern ((has_type x (UINT bits)))
   :qid prelude_unbox_uint
   :skolemid skolem_prelude_unbox_uint
)))
(assert
 (forall ((bits Int) (x Poly)) (!
   (=>
    (has_type x (SINT bits))
    (iInv bits (%I x))
   )
   :pattern ((has_type x (SINT bits)))
   :qid prelude_unbox_sint
   :skolemid skolem_prelude_unbox_sint
)))
(assert
 (forall ((bits Int) (x Poly)) (!
   (=>
    (has_type x (FLOAT bits))
    (uInv bits (%I x))
   )
   :pattern ((has_type x (FLOAT bits)))
   :qid prelude_unbox_float
   :skolemid skolem_prelude_unbox_float
)))
(declare-fun Add (Int Int) Int)
(declare-fun Sub (Int Int) Int)
(declare-fun Mul (Int Int) Int)
(declare-fun EucDiv (Int Int) Int)
(declare-fun EucMod (Int Int) Int)
(declare-fun RAdd (Real Real) Real)
(declare-fun RSub (Real Real) Real)
(declare-fun RMul (Real Real) Real)
(declare-fun RDiv (Real Real) Real)
(assert
 (forall ((x Int) (y Int)) (!
   (= (Add x y) (+ x y))
   :pattern ((Add x y))
   :qid prelude_add
   :skolemid skolem_prelude_add
)))
(assert
 (forall ((x Int) (y Int)) (!
   (= (Sub x y) (- x y))
   :pattern ((Sub x y))
   :qid prelude_sub
   :skolemid skolem_prelude_sub
)))
(assert
 (forall ((x Int) (y Int)) (!
   (= (Mul x y) (* x y))
   :pattern ((Mul x y))
   :qid prelude_mul
   :skolemid skolem_prelude_mul
)))
(assert
 (forall ((x Int) (y Int)) (!
   (= (EucDiv x y) (div x y))
   :pattern ((EucDiv x y))
   :qid prelude_eucdiv
   :skolemid skolem_prelude_eucdiv
)))
(assert
 (forall ((x Int) (y Int)) (!
   (= (EucMod x y) (mod x y))
   :pattern ((EucMod x y))
   :qid prelude_eucmod
   :skolemid skolem_prelude_eucmod
)))
(assert
 (forall ((x Real) (y Real)) (!
   (= (RAdd x y) (+ x y))
   :pattern ((RAdd x y))
   :qid prelude_radd
   :skolemid skolem_prelude_radd
)))
(assert
 (forall ((x Real) (y Real)) (!
   (= (RSub x y) (- x y))
   :pattern ((RSub x y))
   :qid prelude_rsub
   :skolemid skolem_prelude_rsub
)))
(assert
 (forall ((x Real) (y Real)) (!
   (= (RMul x y) (* x y))
   :pattern ((RMul x y))
   :qid prelude_rmul
   :skolemid skolem_prelude_rmul
)))
(assert
 (forall ((x Real) (y Real)) (!
   (= (RDiv x y) (/ x y))
   :pattern ((RDiv x y))
   :qid prelude_rdiv
   :skolemid skolem_prelude_rdiv
)))
(assert
 (forall ((x Int) (y Int)) (!
   (=>
    (and
     (<= 0 x)
     (<= 0 y)
    )
    (<= 0 (Mul x y))
   )
   :pattern ((Mul x y))
   :qid prelude_mul_nats
   :skolemid skolem_prelude_mul_nats
)))
(assert
 (forall ((x Int) (y Int)) (!
   (=>
    (and
     (<= 0 x)
     (< 0 y)
    )
    (and
     (<= 0 (EucDiv x y))
     (<= (EucDiv x y) x)
   ))
   :pattern ((EucDiv x y))
   :qid prelude_div_unsigned_in_bounds
   :skolemid skolem_prelude_div_unsigned_in_bounds
)))
(assert
 (forall ((x Int) (y Int)) (!
   (=>
    (and
     (<= 0 x)
     (< 0 y)
    )
    (and
     (<= 0 (EucMod x y))
     (< (EucMod x y) y)
   ))
   :pattern ((EucMod x y))
   :qid prelude_mod_unsigned_in_bounds
   :skolemid skolem_prelude_mod_unsigned_in_bounds
)))
(declare-fun bitxor (Poly Poly) Int)
(declare-fun bitand (Poly Poly) Int)
(declare-fun bitor (Poly Poly) Int)
(declare-fun bitshr (Poly Poly) Int)
(declare-fun bitshl (Poly Poly) Int)
(declare-fun bitnot (Poly) Int)
(assert
 (forall ((x Poly)) (!
   (= (bitnot x) (- (- 0 (%I x)) 1))
   :pattern ((bitnot x))
   :qid prelude_bit_not
   :skolemid skolem_prelude_bit_not
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (uInv bits (%I x))
     (uInv bits (%I y))
    )
    (uInv bits (bitxor x y))
   )
   :pattern ((uClip bits (bitxor x y)))
   :qid prelude_bit_xor_u_inv
   :skolemid skolem_prelude_bit_xor_u_inv
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (iInv bits (%I x))
     (iInv bits (%I y))
    )
    (iInv bits (bitxor x y))
   )
   :pattern ((iClip bits (bitxor x y)))
   :qid prelude_bit_xor_i_inv
   :skolemid skolem_prelude_bit_xor_i_inv
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (uInv bits (%I x))
     (uInv bits (%I y))
    )
    (uInv bits (bitor x y))
   )
   :pattern ((uClip bits (bitor x y)))
   :qid prelude_bit_or_u_inv
   :skolemid skolem_prelude_bit_or_u_inv
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (iInv bits (%I x))
     (iInv bits (%I y))
    )
    (iInv bits (bitor x y))
   )
   :pattern ((iClip bits (bitor x y)))
   :qid prelude_bit_or_i_inv
   :skolemid skolem_prelude_bit_or_i_inv
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (uInv bits (%I x))
     (uInv bits (%I y))
    )
    (uInv bits (bitand x y))
   )
   :pattern ((uClip bits (bitand x y)))
   :qid prelude_bit_and_u_inv
   :skolemid skolem_prelude_bit_and_u_inv
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (iInv bits (%I x))
     (iInv bits (%I y))
    )
    (iInv bits (bitand x y))
   )
   :pattern ((iClip bits (bitand x y)))
   :qid prelude_bit_and_i_inv
   :skolemid skolem_prelude_bit_and_i_inv
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (uInv bits (%I x))
     (<= 0 (%I y))
    )
    (uInv bits (bitshr x y))
   )
   :pattern ((uClip bits (bitshr x y)))
   :qid prelude_bit_shr_u_inv
   :skolemid skolem_prelude_bit_shr_u_inv
)))
(assert
 (forall ((x Poly) (y Poly) (bits Int)) (!
   (=>
    (and
     (iInv bits (%I x))
     (<= 0 (%I y))
    )
    (iInv bits (bitshr x y))
   )
   :pattern ((iClip bits (bitshr x y)))
   :qid prelude_bit_shr_i_inv
   :skolemid skolem_prelude_bit_shr_i_inv
)))
(declare-fun singular_mod (Int Int) Int)
(assert
 (forall ((x Int) (y Int)) (!
   (=>
    (not (= y 0))
    (= (EucMod x y) (singular_mod x y))
   )
   :pattern ((singular_mod x y))
   :qid prelude_singularmod
   :skolemid skolem_prelude_singularmod
)))
(declare-fun has_resolved (Dcr Type Poly) Bool)
(declare-fun closure_req (Type Dcr Type Poly Poly) Bool)
(declare-fun closure_ens (Type Dcr Type Poly Poly Poly) Bool)
(declare-fun default_ens (Type Dcr Type Poly Poly Poly) Bool)
(declare-fun height (Poly) Height)
(declare-fun height_lt (Height Height) Bool)
(declare-fun fun_from_recursive_field (Poly) Poly)
(declare-fun check_decrease_height (Poly Poly Bool) Bool)
(assert
 (forall ((cur Poly) (prev Poly) (otherwise Bool)) (!
   (= (check_decrease_height cur prev otherwise) (or
     (height_lt (height cur) (height prev))
     (and
      (= (height cur) (height prev))
      otherwise
   )))
   :pattern ((check_decrease_height cur prev otherwise))
   :qid prelude_check_decrease_height
   :skolemid skolem_prelude_check_decrease_height
)))
(assert
 (forall ((cur Int) (prev Int)) (!
   (= (height_lt (height (I cur)) (height (I prev))) (and
     (<= 0 cur)
     (< cur prev)
   ))
   :pattern ((height_lt (height (I cur)) (height (I prev))))
   :qid prelude_check_decrease_int_height
   :skolemid skolem_prelude_check_decrease_int_height
)))
(assert
 (forall ((x Height) (y Height)) (!
   (= (height_lt x y) (and
     ((_ partial-order 0) x y)
     (not (= x y))
   ))
   :pattern ((height_lt x y))
   :qid prelude_height_lt
   :skolemid skolem_prelude_height_lt
)))
(declare-const fuel%vstd!function.axiom_fn_mut_call_requires. FuelId)
(declare-const fuel%vstd!function.axiom_fn_mut_call_ensures. FuelId)
(declare-const fuel%vstd!seq.impl&%2.spec_index. FuelId)
(declare-const fuel%vstd!seq.impl&%2.spec_index_range. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_index_decreases. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_subrange_decreases. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_push_len. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_push_index_same. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_push_index_different. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_ext_equal. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_ext_equal_deep. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_subrange_len. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_subrange_index. FuelId)
(declare-const fuel%vstd!seq.lemma_seq_two_subranges_index. FuelId)
(declare-const fuel%rlimit!above. FuelId)
(declare-const fuel%vstd!array.group_array_axioms. FuelId)
(declare-const fuel%vstd!function.group_function_axioms. FuelId)
(declare-const fuel%vstd!imap.group_imap_lemmas. FuelId)
(declare-const fuel%vstd!iset.group_iset_lemmas. FuelId)
(declare-const fuel%vstd!laws_cmp.group_laws_cmp. FuelId)
(declare-const fuel%vstd!laws_eq.bool_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.u8_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.i8_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.u16_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.i16_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.u32_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.i32_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.u64_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.i64_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.u128_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.i128_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.usize_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.isize_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_1_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_2_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_3_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_4_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_5_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_6_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_7_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_8_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_9_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_10_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_11_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.tuple_12_laws.group_laws_eq. FuelId)
(declare-const fuel%vstd!laws_eq.group_laws_eq. FuelId)
(declare-const fuel%vstd!layout.group_align_properties. FuelId)
(declare-const fuel%vstd!layout.group_layout_axioms. FuelId)
(declare-const fuel%vstd!map.group_map_lemmas. FuelId)
(declare-const fuel%vstd!multiset.group_multiset_axioms. FuelId)
(declare-const fuel%vstd!mut_ref.group_mut_ref_axioms. FuelId)
(declare-const fuel%vstd!raw_ptr.group_raw_ptr_axioms. FuelId)
(declare-const fuel%vstd!seq.group_seq_lemmas. FuelId)
(declare-const fuel%vstd!seq_lib.group_filter_ensures. FuelId)
(declare-const fuel%vstd!seq_lib.group_seq_lib_default. FuelId)
(declare-const fuel%vstd!set.group_set_lemmas. FuelId)
(declare-const fuel%vstd!set_lib.group_set_lib_default. FuelId)
(declare-const fuel%vstd!slice.group_slice_axioms. FuelId)
(declare-const fuel%vstd!string.group_string_axioms. FuelId)
(declare-const fuel%vstd!std_specs.bits.group_bits_axioms. FuelId)
(declare-const fuel%vstd!std_specs.control_flow.group_control_flow_axioms. FuelId)
(declare-const fuel%vstd!std_specs.fmt.group_fmt_axioms. FuelId)
(declare-const fuel%vstd!std_specs.iter.group_iter_axioms. FuelId)
(declare-const fuel%vstd!std_specs.manually_drop.group_manually_drop_axioms. FuelId)
(declare-const fuel%vstd!std_specs.btree.group_btree_axioms. FuelId)
(declare-const fuel%vstd!std_specs.hash.group_hash_axioms. FuelId)
(declare-const fuel%vstd!std_specs.range.group_range_axioms. FuelId)
(declare-const fuel%vstd!std_specs.vec.group_vec_axioms. FuelId)
(declare-const fuel%vstd!std_specs.vecdeque.group_vec_dequeue_axioms. FuelId)
(declare-const fuel%vstd!std_specs.nonzero.group_nonzero_axioms. FuelId)
(declare-const fuel%vstd!group_vstd_default. FuelId)
(assert
 (distinct fuel%vstd!function.axiom_fn_mut_call_requires. fuel%vstd!function.axiom_fn_mut_call_ensures.
  fuel%vstd!seq.impl&%2.spec_index. fuel%vstd!seq.impl&%2.spec_index_range. fuel%vstd!seq.lemma_seq_index_decreases.
  fuel%vstd!seq.lemma_seq_subrange_decreases. fuel%vstd!seq.lemma_seq_push_len. fuel%vstd!seq.lemma_seq_push_index_same.
  fuel%vstd!seq.lemma_seq_push_index_different. fuel%vstd!seq.lemma_seq_ext_equal.
  fuel%vstd!seq.lemma_seq_ext_equal_deep. fuel%vstd!seq.lemma_seq_subrange_len. fuel%vstd!seq.lemma_seq_subrange_index.
  fuel%vstd!seq.lemma_seq_two_subranges_index. fuel%rlimit!above. fuel%vstd!array.group_array_axioms.
  fuel%vstd!function.group_function_axioms. fuel%vstd!imap.group_imap_lemmas. fuel%vstd!iset.group_iset_lemmas.
  fuel%vstd!laws_cmp.group_laws_cmp. fuel%vstd!laws_eq.bool_laws.group_laws_eq. fuel%vstd!laws_eq.u8_laws.group_laws_eq.
  fuel%vstd!laws_eq.i8_laws.group_laws_eq. fuel%vstd!laws_eq.u16_laws.group_laws_eq.
  fuel%vstd!laws_eq.i16_laws.group_laws_eq. fuel%vstd!laws_eq.u32_laws.group_laws_eq.
  fuel%vstd!laws_eq.i32_laws.group_laws_eq. fuel%vstd!laws_eq.u64_laws.group_laws_eq.
  fuel%vstd!laws_eq.i64_laws.group_laws_eq. fuel%vstd!laws_eq.u128_laws.group_laws_eq.
  fuel%vstd!laws_eq.i128_laws.group_laws_eq. fuel%vstd!laws_eq.usize_laws.group_laws_eq.
  fuel%vstd!laws_eq.isize_laws.group_laws_eq. fuel%vstd!laws_eq.tuple_1_laws.group_laws_eq.
  fuel%vstd!laws_eq.tuple_2_laws.group_laws_eq. fuel%vstd!laws_eq.tuple_3_laws.group_laws_eq.
  fuel%vstd!laws_eq.tuple_4_laws.group_laws_eq. fuel%vstd!laws_eq.tuple_5_laws.group_laws_eq.
  fuel%vstd!laws_eq.tuple_6_laws.group_laws_eq. fuel%vstd!laws_eq.tuple_7_laws.group_laws_eq.
  fuel%vstd!laws_eq.tuple_8_laws.group_laws_eq. fuel%vstd!laws_eq.tuple_9_laws.group_laws_eq.
  fuel%vstd!laws_eq.tuple_10_laws.group_laws_eq. fuel%vstd!laws_eq.tuple_11_laws.group_laws_eq.
  fuel%vstd!laws_eq.tuple_12_laws.group_laws_eq. fuel%vstd!laws_eq.group_laws_eq. fuel%vstd!layout.group_align_properties.
  fuel%vstd!layout.group_layout_axioms. fuel%vstd!map.group_map_lemmas. fuel%vstd!multiset.group_multiset_axioms.
  fuel%vstd!mut_ref.group_mut_ref_axioms. fuel%vstd!raw_ptr.group_raw_ptr_axioms. fuel%vstd!seq.group_seq_lemmas.
  fuel%vstd!seq_lib.group_filter_ensures. fuel%vstd!seq_lib.group_seq_lib_default.
  fuel%vstd!set.group_set_lemmas. fuel%vstd!set_lib.group_set_lib_default. fuel%vstd!slice.group_slice_axioms.
  fuel%vstd!string.group_string_axioms. fuel%vstd!std_specs.bits.group_bits_axioms.
  fuel%vstd!std_specs.control_flow.group_control_flow_axioms. fuel%vstd!std_specs.fmt.group_fmt_axioms.
  fuel%vstd!std_specs.iter.group_iter_axioms. fuel%vstd!std_specs.manually_drop.group_manually_drop_axioms.
  fuel%vstd!std_specs.btree.group_btree_axioms. fuel%vstd!std_specs.hash.group_hash_axioms.
  fuel%vstd!std_specs.range.group_range_axioms. fuel%vstd!std_specs.vec.group_vec_axioms.
  fuel%vstd!std_specs.vecdeque.group_vec_dequeue_axioms. fuel%vstd!std_specs.nonzero.group_nonzero_axioms.
  fuel%vstd!group_vstd_default.
))
(assert
 (=>
  (fuel_bool_default fuel%vstd!function.group_function_axioms.)
  (and
   (fuel_bool_default fuel%vstd!function.axiom_fn_mut_call_requires.)
   (fuel_bool_default fuel%vstd!function.axiom_fn_mut_call_ensures.)
)))
(assert
 (=>
  (fuel_bool_default fuel%vstd!laws_eq.group_laws_eq.)
  (and
   (fuel_bool_default fuel%vstd!laws_eq.bool_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.u8_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.i8_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.u16_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.i16_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.u32_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.i32_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.u64_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.i64_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.u128_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.i128_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.usize_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.isize_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_1_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_2_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_3_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_4_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_5_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_6_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_7_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_8_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_9_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_10_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_11_laws.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_eq.tuple_12_laws.group_laws_eq.)
)))
(assert
 (=>
  (fuel_bool_default fuel%vstd!layout.group_layout_axioms.)
  (fuel_bool_default fuel%vstd!layout.group_align_properties.)
))
(assert
 (=>
  (fuel_bool_default fuel%vstd!seq.group_seq_lemmas.)
  (and
   (fuel_bool_default fuel%vstd!seq.lemma_seq_index_decreases.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_subrange_decreases.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_push_len.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_push_index_same.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_push_index_different.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_ext_equal.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_ext_equal_deep.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_subrange_len.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_subrange_index.)
   (fuel_bool_default fuel%vstd!seq.lemma_seq_two_subranges_index.)
)))
(assert
 (=>
  (fuel_bool_default fuel%vstd!seq_lib.group_seq_lib_default.)
  (fuel_bool_default fuel%vstd!seq_lib.group_filter_ensures.)
))
(assert
 (fuel_bool_default fuel%vstd!group_vstd_default.)
)
(assert
 (=>
  (fuel_bool_default fuel%vstd!group_vstd_default.)
  (and
   (fuel_bool_default fuel%vstd!seq.group_seq_lemmas.)
   (fuel_bool_default fuel%vstd!seq_lib.group_seq_lib_default.)
   (fuel_bool_default fuel%vstd!map.group_map_lemmas.)
   (fuel_bool_default fuel%vstd!set.group_set_lemmas.)
   (fuel_bool_default fuel%vstd!imap.group_imap_lemmas.)
   (fuel_bool_default fuel%vstd!iset.group_iset_lemmas.)
   (fuel_bool_default fuel%vstd!set_lib.group_set_lib_default.)
   (fuel_bool_default fuel%vstd!multiset.group_multiset_axioms.)
   (fuel_bool_default fuel%vstd!function.group_function_axioms.)
   (fuel_bool_default fuel%vstd!laws_eq.group_laws_eq.)
   (fuel_bool_default fuel%vstd!laws_cmp.group_laws_cmp.)
   (fuel_bool_default fuel%vstd!slice.group_slice_axioms.)
   (fuel_bool_default fuel%vstd!array.group_array_axioms.)
   (fuel_bool_default fuel%vstd!string.group_string_axioms.)
   (fuel_bool_default fuel%vstd!raw_ptr.group_raw_ptr_axioms.)
   (fuel_bool_default fuel%vstd!layout.group_layout_axioms.)
   (fuel_bool_default fuel%vstd!mut_ref.group_mut_ref_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.range.group_range_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.bits.group_bits_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.control_flow.group_control_flow_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.fmt.group_fmt_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.manually_drop.group_manually_drop_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.iter.group_iter_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.vec.group_vec_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.vecdeque.group_vec_dequeue_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.hash.group_hash_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.btree.group_btree_axioms.)
   (fuel_bool_default fuel%vstd!std_specs.nonzero.group_nonzero_axioms.)
)))
(declare-fun tr_bound%core!clone.Clone. (Dcr Type) Bool)
(declare-fun tr_bound%core!marker.Copy. (Dcr Type) Bool)
(declare-fun tr_bound%core!marker.Tuple. (Dcr Type) Bool)
(declare-fun tr_bound%core!ops.function.FnOnce. (Dcr Type Dcr Type) Bool)
(declare-fun tr_bound%core!ops.function.FnMut. (Dcr Type Dcr Type) Bool)
(declare-fun tr_bound%core!ops.function.Fn. (Dcr Type Dcr Type) Bool)
(declare-fun tr_bound%verus_builtin!Integer. (Dcr Type) Bool)
(declare-fun tr_bound%core!alloc.Allocator. (Dcr Type) Bool)
(declare-fun proj%%core!ops.function.FnOnce./Output (Dcr Type Dcr Type) Dcr)
(declare-fun proj%core!ops.function.FnOnce./Output (Dcr Type Dcr Type) Type)
(declare-sort vstd!seq.Seq<int.>. 0)
(declare-datatypes ((tuple%0. 0)) (((tuple%0./tuple%0))))
(declare-fun TYPE%vstd!seq.Seq. (Dcr Type) Type)
(declare-fun Poly%vstd!seq.Seq<int.>. (vstd!seq.Seq<int.>.) Poly)
(declare-fun %Poly%vstd!seq.Seq<int.>. (Poly) vstd!seq.Seq<int.>.)
(declare-fun Poly%tuple%0. (tuple%0.) Poly)
(declare-fun %Poly%tuple%0. (Poly) tuple%0.)
(assert
 (forall ((x vstd!seq.Seq<int.>.)) (!
   (= x (%Poly%vstd!seq.Seq<int.>. (Poly%vstd!seq.Seq<int.>. x)))
   :pattern ((Poly%vstd!seq.Seq<int.>. x))
   :qid internal_vstd__seq__Seq<int.>_box_axiom_definition
   :skolemid skolem_internal_vstd__seq__Seq<int.>_box_axiom_definition
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x (TYPE%vstd!seq.Seq. $ INT))
    (= x (Poly%vstd!seq.Seq<int.>. (%Poly%vstd!seq.Seq<int.>. x)))
   )
   :pattern ((has_type x (TYPE%vstd!seq.Seq. $ INT)))
   :qid internal_vstd__seq__Seq<int.>_unbox_axiom_definition
   :skolemid skolem_internal_vstd__seq__Seq<int.>_unbox_axiom_definition
)))
(assert
 (forall ((x vstd!seq.Seq<int.>.)) (!
   (has_type (Poly%vstd!seq.Seq<int.>. x) (TYPE%vstd!seq.Seq. $ INT))
   :pattern ((has_type (Poly%vstd!seq.Seq<int.>. x) (TYPE%vstd!seq.Seq. $ INT)))
   :qid internal_vstd__seq__Seq<int.>_has_type_always_definition
   :skolemid skolem_internal_vstd__seq__Seq<int.>_has_type_always_definition
)))
(assert
 (forall ((x tuple%0.)) (!
   (= x (%Poly%tuple%0. (Poly%tuple%0. x)))
   :pattern ((Poly%tuple%0. x))
   :qid internal_crate__tuple__0_box_axiom_definition
   :skolemid skolem_internal_crate__tuple__0_box_axiom_definition
)))
(assert
 (forall ((x Poly)) (!
   (=>
    (has_type x TYPE%tuple%0.)
    (= x (Poly%tuple%0. (%Poly%tuple%0. x)))
   )
   :pattern ((has_type x TYPE%tuple%0.))
   :qid internal_crate__tuple__0_unbox_axiom_definition
   :skolemid skolem_internal_crate__tuple__0_unbox_axiom_definition
)))
(assert
 (forall ((x tuple%0.)) (!
   (has_type (Poly%tuple%0. x) TYPE%tuple%0.)
   :pattern ((has_type (Poly%tuple%0. x) TYPE%tuple%0.))
   :qid internal_crate__tuple__0_has_type_always_definition
   :skolemid skolem_internal_crate__tuple__0_has_type_always_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type)) (!
   (=>
    (tr_bound%core!clone.Clone. Self%&. Self%&)
    (sized Self%&.)
   )
   :pattern ((tr_bound%core!clone.Clone. Self%&. Self%&))
   :qid internal_core__clone__Clone_trait_type_bounds_definition
   :skolemid skolem_internal_core__clone__Clone_trait_type_bounds_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type)) (!
   (=>
    (tr_bound%core!marker.Copy. Self%&. Self%&)
    (tr_bound%core!clone.Clone. Self%&. Self%&)
   )
   :pattern ((tr_bound%core!marker.Copy. Self%&. Self%&))
   :qid internal_core__marker__Copy_trait_type_bounds_definition
   :skolemid skolem_internal_core__marker__Copy_trait_type_bounds_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type)) (!
   true
   :pattern ((tr_bound%core!marker.Tuple. Self%&. Self%&))
   :qid internal_core__marker__Tuple_trait_type_bounds_definition
   :skolemid skolem_internal_core__marker__Tuple_trait_type_bounds_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type) (Args&. Dcr) (Args& Type)) (!
   (=>
    (tr_bound%core!ops.function.FnOnce. Self%&. Self%& Args&. Args&)
    (and
     (sized Args&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
     (sized (proj%%core!ops.function.FnOnce./Output Self%&. Self%& Args&. Args&))
   ))
   :pattern ((tr_bound%core!ops.function.FnOnce. Self%&. Self%& Args&. Args&))
   :qid internal_core__ops__function__FnOnce_trait_type_bounds_definition
   :skolemid skolem_internal_core__ops__function__FnOnce_trait_type_bounds_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type) (Args&. Dcr) (Args& Type)) (!
   (=>
    (tr_bound%core!ops.function.FnMut. Self%&. Self%& Args&. Args&)
    (and
     (tr_bound%core!ops.function.FnOnce. Self%&. Self%& Args&. Args&)
     (sized Args&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
   ))
   :pattern ((tr_bound%core!ops.function.FnMut. Self%&. Self%& Args&. Args&))
   :qid internal_core__ops__function__FnMut_trait_type_bounds_definition
   :skolemid skolem_internal_core__ops__function__FnMut_trait_type_bounds_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type) (Args&. Dcr) (Args& Type)) (!
   (=>
    (tr_bound%core!ops.function.Fn. Self%&. Self%& Args&. Args&)
    (and
     (tr_bound%core!ops.function.FnMut. Self%&. Self%& Args&. Args&)
     (sized Args&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
   ))
   :pattern ((tr_bound%core!ops.function.Fn. Self%&. Self%& Args&. Args&))
   :qid internal_core__ops__function__Fn_trait_type_bounds_definition
   :skolemid skolem_internal_core__ops__function__Fn_trait_type_bounds_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type)) (!
   (=>
    (tr_bound%verus_builtin!Integer. Self%&. Self%&)
    (tr_bound%core!marker.Copy. Self%&. Self%&)
   )
   :pattern ((tr_bound%verus_builtin!Integer. Self%&. Self%&))
   :qid internal_verus_builtin__Integer_trait_type_bounds_definition
   :skolemid skolem_internal_verus_builtin__Integer_trait_type_bounds_definition
)))
(assert
 (forall ((Self%&. Dcr) (Self%& Type)) (!
   true
   :pattern ((tr_bound%core!alloc.Allocator. Self%&. Self%&))
   :qid internal_core__alloc__Allocator_trait_type_bounds_definition
   :skolemid skolem_internal_core__alloc__Allocator_trait_type_bounds_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.Fn. F&. F& A&. A&)
    )
    (= (proj%%core!ops.function.FnOnce./Output (REF F&.) F& A&. A&) (proj%%core!ops.function.FnOnce./Output
      F&. F& A&. A&
   )))
   :pattern ((proj%%core!ops.function.FnOnce./Output (REF F&.) F& A&. A&))
   :qid internal_proj____core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__2_assoc_type_impl_true_definition
   :skolemid skolem_internal_proj____core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__2_assoc_type_impl_true_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.Fn. F&. F& A&. A&)
    )
    (= (proj%core!ops.function.FnOnce./Output (REF F&.) F& A&. A&) (proj%core!ops.function.FnOnce./Output
      F&. F& A&. A&
   )))
   :pattern ((proj%core!ops.function.FnOnce./Output (REF F&.) F& A&. A&))
   :qid internal_proj__core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__2_assoc_type_impl_false_definition
   :skolemid skolem_internal_proj__core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__2_assoc_type_impl_false_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.FnMut. F&. F& A&. A&)
    )
    (= (proj%%core!ops.function.FnOnce./Output $ (MUTREF F&. F&) A&. A&) (proj%%core!ops.function.FnOnce./Output
      F&. F& A&. A&
   )))
   :pattern ((proj%%core!ops.function.FnOnce./Output $ (MUTREF F&. F&) A&. A&))
   :qid internal_proj____core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__4_assoc_type_impl_true_definition
   :skolemid skolem_internal_proj____core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__4_assoc_type_impl_true_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.FnMut. F&. F& A&. A&)
    )
    (= (proj%core!ops.function.FnOnce./Output $ (MUTREF F&. F&) A&. A&) (proj%core!ops.function.FnOnce./Output
      F&. F& A&. A&
   )))
   :pattern ((proj%core!ops.function.FnOnce./Output $ (MUTREF F&. F&) A&. A&))
   :qid internal_proj__core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__4_assoc_type_impl_false_definition
   :skolemid skolem_internal_proj__core!ops.function.FnOnce./Output_core__ops__function__impls__impl&__4_assoc_type_impl_false_definition
)))
(assert
 (forall ((Args&. Dcr) (Args& Type) (F&. Dcr) (F& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized Args&.)
     (sized A&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
     (tr_bound%core!ops.function.FnOnce. F&. F& Args&. Args&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (= (proj%%core!ops.function.FnOnce./Output (BOX A&. A& F&.) F& Args&. Args&) (proj%%core!ops.function.FnOnce./Output
      F&. F& Args&. Args&
   )))
   :pattern ((proj%%core!ops.function.FnOnce./Output (BOX A&. A& F&.) F& Args&. Args&))
   :qid internal_proj____core!ops.function.FnOnce./Output_alloc__boxed__impl&__31_assoc_type_impl_true_definition
   :skolemid skolem_internal_proj____core!ops.function.FnOnce./Output_alloc__boxed__impl&__31_assoc_type_impl_true_definition
)))
(assert
 (forall ((Args&. Dcr) (Args& Type) (F&. Dcr) (F& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized Args&.)
     (sized A&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
     (tr_bound%core!ops.function.FnOnce. F&. F& Args&. Args&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (= (proj%core!ops.function.FnOnce./Output (BOX A&. A& F&.) F& Args&. Args&) (proj%core!ops.function.FnOnce./Output
      F&. F& Args&. Args&
   )))
   :pattern ((proj%core!ops.function.FnOnce./Output (BOX A&. A& F&.) F& Args&. Args&))
   :qid internal_proj__core!ops.function.FnOnce./Output_alloc__boxed__impl&__31_assoc_type_impl_false_definition
   :skolemid skolem_internal_proj__core!ops.function.FnOnce./Output_alloc__boxed__impl&__31_assoc_type_impl_false_definition
)))
(declare-fun vstd!seq.Seq.len.? (Dcr Type Poly) Int)
(declare-fun vstd!seq.Seq.index.? (Dcr Type Poly Poly) Poly)
(declare-fun vstd!seq.impl&%2.spec_index.? (Dcr Type Poly Poly) Poly)
(declare-fun vstd!seq.Seq.subrange.? (Dcr Type Poly Poly Poly) Poly)
(declare-fun vstd!seq.impl&%2.spec_index_range.? (Dcr Type Dcr Type Dcr Type Poly Poly
  Poly
 ) Poly
)
(declare-fun vstd!seq.Seq.push.? (Dcr Type Poly Poly) Poly)
(declare-fun rlimit!above.? (Poly Poly) Bool)
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly)) (!
   (=>
    (has_type self! (TYPE%vstd!seq.Seq. A&. A&))
    (<= 0 (vstd!seq.Seq.len.? A&. A& self!))
   )
   :pattern ((vstd!seq.Seq.len.? A&. A& self!))
   :qid internal_vstd!seq.Seq.len.?_pre_post_definition
   :skolemid skolem_internal_vstd!seq.Seq.len.?_pre_post_definition
)))
(declare-fun req%vstd!seq.Seq.index. (Dcr Type Poly Poly) Bool)
(declare-const %%global_location_label%%0 Bool)
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly) (i! Poly)) (!
   (= (req%vstd!seq.Seq.index. A&. A& self! i!) (=>
     %%global_location_label%%0
     (let
      ((tmp%%$ 0))
      (let
       ((tmp%%$1 (%I i!)))
       (let
        ((tmp%%$2 (vstd!seq.Seq.len.? A&. A& self!)))
        (and
         (<= tmp%%$ tmp%%$1)
         (< tmp%%$1 tmp%%$2)
   ))))))
   :pattern ((req%vstd!seq.Seq.index. A&. A& self! i!))
   :qid internal_req__vstd!seq.Seq.index._definition
   :skolemid skolem_internal_req__vstd!seq.Seq.index._definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly) (i! Poly)) (!
   (=>
    (and
     (has_type self! (TYPE%vstd!seq.Seq. A&. A&))
     (has_type i! INT)
    )
    (has_type (vstd!seq.Seq.index.? A&. A& self! i!) A&)
   )
   :pattern ((vstd!seq.Seq.index.? A&. A& self! i!))
   :qid internal_vstd!seq.Seq.index.?_pre_post_definition
   :skolemid skolem_internal_vstd!seq.Seq.index.?_pre_post_definition
)))
(declare-fun req%vstd!seq.impl&%2.spec_index. (Dcr Type Poly Poly) Bool)
(declare-const %%global_location_label%%1 Bool)
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly) (i! Poly)) (!
   (= (req%vstd!seq.impl&%2.spec_index. A&. A& self! i!) (=>
     %%global_location_label%%1
     (let
      ((tmp%%$ 0))
      (let
       ((tmp%%$1 (%I i!)))
       (let
        ((tmp%%$2 (vstd!seq.Seq.len.? A&. A& self!)))
        (and
         (<= tmp%%$ tmp%%$1)
         (< tmp%%$1 tmp%%$2)
   ))))))
   :pattern ((req%vstd!seq.impl&%2.spec_index. A&. A& self! i!))
   :qid internal_req__vstd!seq.impl&__2.spec_index._definition
   :skolemid skolem_internal_req__vstd!seq.impl&__2.spec_index._definition
)))
(assert
 (fuel_bool_default fuel%vstd!seq.impl&%2.spec_index.)
)
(assert
 (=>
  (fuel_bool fuel%vstd!seq.impl&%2.spec_index.)
  (forall ((A&. Dcr) (A& Type) (self! Poly) (i! Poly)) (!
    (= (vstd!seq.impl&%2.spec_index.? A&. A& self! i!) (vstd!seq.Seq.index.? A&. A& self!
      i!
    ))
    :pattern ((vstd!seq.impl&%2.spec_index.? A&. A& self! i!))
    :qid internal_vstd!seq.impl&__2.spec_index.?_definition
    :skolemid skolem_internal_vstd!seq.impl&__2.spec_index.?_definition
))))
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly) (i! Poly)) (!
   (=>
    (and
     (has_type self! (TYPE%vstd!seq.Seq. A&. A&))
     (has_type i! INT)
    )
    (has_type (vstd!seq.impl&%2.spec_index.? A&. A& self! i!) A&)
   )
   :pattern ((vstd!seq.impl&%2.spec_index.? A&. A& self! i!))
   :qid internal_vstd!seq.impl&__2.spec_index.?_pre_post_definition
   :skolemid skolem_internal_vstd!seq.impl&__2.spec_index.?_pre_post_definition
)))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_index_decreases.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (i! Poly)) (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type i! INT)
     )
     (=>
      (and
       (sized A&.)
       (let
        ((tmp%%$ 0))
        (let
         ((tmp%%$1 (%I i!)))
         (let
          ((tmp%%$2 (vstd!seq.Seq.len.? A&. A& s!)))
          (and
           (<= tmp%%$ tmp%%$1)
           (< tmp%%$1 tmp%%$2)
      )))))
      (height_lt (height (vstd!seq.Seq.index.? A&. A& s! i!)) (height s!))
    ))
    :pattern ((height (vstd!seq.Seq.index.? A&. A& s! i!)))
    :qid user_vstd__seq__lemma_seq_index_decreases_0
    :skolemid skolem_user_vstd__seq__lemma_seq_index_decreases_0
))))
(assert
 (tr_bound%core!clone.Clone. $ INT)
)
(assert
 (tr_bound%core!marker.Copy. $ INT)
)
(assert
 (tr_bound%verus_builtin!Integer. $ INT)
)
(declare-fun req%vstd!seq.Seq.subrange. (Dcr Type Poly Poly Poly) Bool)
(declare-const %%global_location_label%%2 Bool)
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly) (start_inclusive! Poly) (end_exclusive! Poly))
  (!
   (= (req%vstd!seq.Seq.subrange. A&. A& self! start_inclusive! end_exclusive!) (=>
     %%global_location_label%%2
     (let
      ((tmp%%$ 0))
      (let
       ((tmp%%$1 (%I start_inclusive!)))
       (let
        ((tmp%%$2 (%I end_exclusive!)))
        (let
         ((tmp%%$3 (vstd!seq.Seq.len.? A&. A& self!)))
         (and
          (and
           (<= tmp%%$ tmp%%$1)
           (<= tmp%%$1 tmp%%$2)
          )
          (<= tmp%%$2 tmp%%$3)
   )))))))
   :pattern ((req%vstd!seq.Seq.subrange. A&. A& self! start_inclusive! end_exclusive!))
   :qid internal_req__vstd!seq.Seq.subrange._definition
   :skolemid skolem_internal_req__vstd!seq.Seq.subrange._definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly) (start_inclusive! Poly) (end_exclusive! Poly))
  (!
   (=>
    (and
     (has_type self! (TYPE%vstd!seq.Seq. A&. A&))
     (has_type start_inclusive! INT)
     (has_type end_exclusive! INT)
    )
    (has_type (vstd!seq.Seq.subrange.? A&. A& self! start_inclusive! end_exclusive!) (
      TYPE%vstd!seq.Seq. A&. A&
   )))
   :pattern ((vstd!seq.Seq.subrange.? A&. A& self! start_inclusive! end_exclusive!))
   :qid internal_vstd!seq.Seq.subrange.?_pre_post_definition
   :skolemid skolem_internal_vstd!seq.Seq.subrange.?_pre_post_definition
)))
(declare-fun req%vstd!seq.impl&%2.spec_index_range. (Dcr Type Dcr Type Dcr Type Poly
  Poly Poly
 ) Bool
)
(declare-const %%global_location_label%%3 Bool)
(assert
 (forall ((A&. Dcr) (A& Type) (I&. Dcr) (I& Type) (J&. Dcr) (J& Type) (self! Poly) (
    i! Poly
   ) (j! Poly)
  ) (!
   (= (req%vstd!seq.impl&%2.spec_index_range. A&. A& I&. I& J&. J& self! i! j!) (=>
     %%global_location_label%%3
     (let
      ((tmp%%$ 0))
      (let
       ((tmp%%$1 (%I i!)))
       (let
        ((tmp%%$2 (%I j!)))
        (let
         ((tmp%%$3 (vstd!seq.Seq.len.? A&. A& self!)))
         (and
          (and
           (<= tmp%%$ tmp%%$1)
           (<= tmp%%$1 tmp%%$2)
          )
          (<= tmp%%$2 tmp%%$3)
   )))))))
   :pattern ((req%vstd!seq.impl&%2.spec_index_range. A&. A& I&. I& J&. J& self! i! j!))
   :qid internal_req__vstd!seq.impl&__2.spec_index_range._definition
   :skolemid skolem_internal_req__vstd!seq.impl&__2.spec_index_range._definition
)))
(assert
 (fuel_bool_default fuel%vstd!seq.impl&%2.spec_index_range.)
)
(assert
 (=>
  (fuel_bool fuel%vstd!seq.impl&%2.spec_index_range.)
  (forall ((A&. Dcr) (A& Type) (I&. Dcr) (I& Type) (J&. Dcr) (J& Type) (self! Poly) (
     i! Poly
    ) (j! Poly)
   ) (!
    (= (vstd!seq.impl&%2.spec_index_range.? A&. A& I&. I& J&. J& self! i! j!) (vstd!seq.Seq.subrange.?
      A&. A& self! i! j!
    ))
    :pattern ((vstd!seq.impl&%2.spec_index_range.? A&. A& I&. I& J&. J& self! i! j!))
    :qid internal_vstd!seq.impl&__2.spec_index_range.?_definition
    :skolemid skolem_internal_vstd!seq.impl&__2.spec_index_range.?_definition
))))
(assert
 (forall ((A&. Dcr) (A& Type) (I&. Dcr) (I& Type) (J&. Dcr) (J& Type) (self! Poly) (
    i! Poly
   ) (j! Poly)
  ) (!
   (=>
    (and
     (has_type self! (TYPE%vstd!seq.Seq. A&. A&))
     (has_type i! I&)
     (has_type j! J&)
    )
    (has_type (vstd!seq.impl&%2.spec_index_range.? A&. A& I&. I& J&. J& self! i! j!) (
      TYPE%vstd!seq.Seq. A&. A&
   )))
   :pattern ((vstd!seq.impl&%2.spec_index_range.? A&. A& I&. I& J&. J& self! i! j!))
   :qid internal_vstd!seq.impl&__2.spec_index_range.?_pre_post_definition
   :skolemid skolem_internal_vstd!seq.impl&__2.spec_index_range.?_pre_post_definition
)))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_subrange_decreases.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (i! Poly) (j! Poly)) (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type i! INT)
      (has_type j! INT)
     )
     (=>
      (and
       (and
        (sized A&.)
        (let
         ((tmp%%$ 0))
         (let
          ((tmp%%$1 (%I i!)))
          (let
           ((tmp%%$2 (%I j!)))
           (let
            ((tmp%%$3 (vstd!seq.Seq.len.? A&. A& s!)))
            (and
             (and
              (<= tmp%%$ tmp%%$1)
              (<= tmp%%$1 tmp%%$2)
             )
             (<= tmp%%$2 tmp%%$3)
       ))))))
       (< (vstd!seq.Seq.len.? A&. A& (vstd!seq.Seq.subrange.? A&. A& s! i! j!)) (vstd!seq.Seq.len.?
         A&. A& s!
      )))
      (height_lt (height (vstd!seq.Seq.subrange.? A&. A& s! i! j!)) (height s!))
    ))
    :pattern ((height (vstd!seq.Seq.subrange.? A&. A& s! i! j!)))
    :qid user_vstd__seq__lemma_seq_subrange_decreases_0
    :skolemid skolem_user_vstd__seq__lemma_seq_subrange_decreases_0
))))
(assert
 (forall ((A&. Dcr) (A& Type) (self! Poly) (a! Poly)) (!
   (=>
    (and
     (has_type self! (TYPE%vstd!seq.Seq. A&. A&))
     (has_type a! A&)
    )
    (has_type (vstd!seq.Seq.push.? A&. A& self! a!) (TYPE%vstd!seq.Seq. A&. A&))
   )
   :pattern ((vstd!seq.Seq.push.? A&. A& self! a!))
   :qid internal_vstd!seq.Seq.push.?_pre_post_definition
   :skolemid skolem_internal_vstd!seq.Seq.push.?_pre_post_definition
)))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_push_len.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (a! Poly)) (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type a! A&)
     )
     (=>
      (sized A&.)
      (= (vstd!seq.Seq.len.? A&. A& (vstd!seq.Seq.push.? A&. A& s! a!)) (nClip (Add (vstd!seq.Seq.len.?
          A&. A& s!
         ) 1
    )))))
    :pattern ((vstd!seq.Seq.len.? A&. A& (vstd!seq.Seq.push.? A&. A& s! a!)))
    :qid user_vstd__seq__lemma_seq_push_len_0
    :skolemid skolem_user_vstd__seq__lemma_seq_push_len_0
))))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_push_index_same.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (a! Poly) (i! Poly)) (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type a! A&)
      (has_type i! INT)
     )
     (=>
      (and
       (sized A&.)
       (= (%I i!) (vstd!seq.Seq.len.? A&. A& s!))
      )
      (= (vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.push.? A&. A& s! a!) i!) a!)
    ))
    :pattern ((vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.push.? A&. A& s! a!) i!))
    :qid user_vstd__seq__lemma_seq_push_index_same_0
    :skolemid skolem_user_vstd__seq__lemma_seq_push_index_same_0
))))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_push_index_different.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (a! Poly) (i! Poly)) (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type a! A&)
      (has_type i! INT)
     )
     (=>
      (and
       (sized A&.)
       (< (%I i!) (vstd!seq.Seq.len.? A&. A& s!))
      )
      (= (vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.push.? A&. A& s! a!) i!) (vstd!seq.Seq.index.?
        A&. A& s! i!
    ))))
    :pattern ((vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.push.? A&. A& s! a!) i!))
    :qid user_vstd__seq__lemma_seq_push_index_different_0
    :skolemid skolem_user_vstd__seq__lemma_seq_push_index_different_0
))))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_ext_equal.)
  (forall ((A&. Dcr) (A& Type) (s1! Poly) (s2! Poly)) (!
    (=>
     (and
      (has_type s1! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type s2! (TYPE%vstd!seq.Seq. A&. A&))
     )
     (=>
      (sized A&.)
      (= (ext_eq false (TYPE%vstd!seq.Seq. A&. A&) s1! s2!) (and
        (= (vstd!seq.Seq.len.? A&. A& s1!) (vstd!seq.Seq.len.? A&. A& s2!))
        (forall ((i$ Poly)) (!
          (=>
           (has_type i$ INT)
           (=>
            (let
             ((tmp%%$ 0))
             (let
              ((tmp%%$1 (%I i$)))
              (let
               ((tmp%%$2 (vstd!seq.Seq.len.? A&. A& s1!)))
               (and
                (<= tmp%%$ tmp%%$1)
                (< tmp%%$1 tmp%%$2)
            ))))
            (= (vstd!seq.Seq.index.? A&. A& s1! i$) (vstd!seq.Seq.index.? A&. A& s2! i$))
          ))
          :pattern ((vstd!seq.Seq.index.? A&. A& s1! i$))
          :pattern ((vstd!seq.Seq.index.? A&. A& s2! i$))
          :qid user_vstd__seq__lemma_seq_ext_equal_0
          :skolemid skolem_user_vstd__seq__lemma_seq_ext_equal_0
    ))))))
    :pattern ((ext_eq false (TYPE%vstd!seq.Seq. A&. A&) s1! s2!))
    :qid user_vstd__seq__lemma_seq_ext_equal_1
    :skolemid skolem_user_vstd__seq__lemma_seq_ext_equal_1
))))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_ext_equal_deep.)
  (forall ((A&. Dcr) (A& Type) (s1! Poly) (s2! Poly)) (!
    (=>
     (and
      (has_type s1! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type s2! (TYPE%vstd!seq.Seq. A&. A&))
     )
     (=>
      (sized A&.)
      (= (ext_eq true (TYPE%vstd!seq.Seq. A&. A&) s1! s2!) (and
        (= (vstd!seq.Seq.len.? A&. A& s1!) (vstd!seq.Seq.len.? A&. A& s2!))
        (forall ((i$ Poly)) (!
          (=>
           (has_type i$ INT)
           (=>
            (let
             ((tmp%%$ 0))
             (let
              ((tmp%%$1 (%I i$)))
              (let
               ((tmp%%$2 (vstd!seq.Seq.len.? A&. A& s1!)))
               (and
                (<= tmp%%$ tmp%%$1)
                (< tmp%%$1 tmp%%$2)
            ))))
            (ext_eq true A& (vstd!seq.Seq.index.? A&. A& s1! i$) (vstd!seq.Seq.index.? A&. A& s2!
              i$
          ))))
          :pattern ((vstd!seq.Seq.index.? A&. A& s1! i$))
          :pattern ((vstd!seq.Seq.index.? A&. A& s2! i$))
          :qid user_vstd__seq__lemma_seq_ext_equal_deep_0
          :skolemid skolem_user_vstd__seq__lemma_seq_ext_equal_deep_0
    ))))))
    :pattern ((ext_eq true (TYPE%vstd!seq.Seq. A&. A&) s1! s2!))
    :qid user_vstd__seq__lemma_seq_ext_equal_deep_1
    :skolemid skolem_user_vstd__seq__lemma_seq_ext_equal_deep_1
))))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_subrange_len.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (j! Poly) (k! Poly)) (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type j! INT)
      (has_type k! INT)
     )
     (=>
      (and
       (sized A&.)
       (let
        ((tmp%%$ 0))
        (let
         ((tmp%%$1 (%I j!)))
         (let
          ((tmp%%$2 (%I k!)))
          (let
           ((tmp%%$3 (vstd!seq.Seq.len.? A&. A& s!)))
           (and
            (and
             (<= tmp%%$ tmp%%$1)
             (<= tmp%%$1 tmp%%$2)
            )
            (<= tmp%%$2 tmp%%$3)
      ))))))
      (= (vstd!seq.Seq.len.? A&. A& (vstd!seq.Seq.subrange.? A&. A& s! j! k!)) (Sub (%I k!)
        (%I j!)
    ))))
    :pattern ((vstd!seq.Seq.len.? A&. A& (vstd!seq.Seq.subrange.? A&. A& s! j! k!)))
    :qid user_vstd__seq__lemma_seq_subrange_len_0
    :skolemid skolem_user_vstd__seq__lemma_seq_subrange_len_0
))))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_subrange_index.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (j! Poly) (k! Poly) (i! Poly)) (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type j! INT)
      (has_type k! INT)
      (has_type i! INT)
     )
     (=>
      (and
       (and
        (sized A&.)
        (let
         ((tmp%%$ 0))
         (let
          ((tmp%%$1 (%I j!)))
          (let
           ((tmp%%$2 (%I k!)))
           (let
            ((tmp%%$3 (vstd!seq.Seq.len.? A&. A& s!)))
            (and
             (and
              (<= tmp%%$ tmp%%$1)
              (<= tmp%%$1 tmp%%$2)
             )
             (<= tmp%%$2 tmp%%$3)
       ))))))
       (let
        ((tmp%%$ 0))
        (let
         ((tmp%%$5 (%I i!)))
         (let
          ((tmp%%$6 (Sub (%I k!) (%I j!))))
          (and
           (<= tmp%%$ tmp%%$5)
           (< tmp%%$5 tmp%%$6)
      )))))
      (= (vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.subrange.? A&. A& s! j! k!) i!) (vstd!seq.Seq.index.?
        A&. A& s! (I (Add (%I i!) (%I j!)))
    ))))
    :pattern ((vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.subrange.? A&. A& s! j! k!) i!))
    :qid user_vstd__seq__lemma_seq_subrange_index_0
    :skolemid skolem_user_vstd__seq__lemma_seq_subrange_index_0
))))
(assert
 (=>
  (fuel_bool fuel%vstd!seq.lemma_seq_two_subranges_index.)
  (forall ((A&. Dcr) (A& Type) (s! Poly) (j! Poly) (k1! Poly) (k2! Poly) (i! Poly))
   (!
    (=>
     (and
      (has_type s! (TYPE%vstd!seq.Seq. A&. A&))
      (has_type j! INT)
      (has_type k1! INT)
      (has_type k2! INT)
      (has_type i! INT)
     )
     (=>
      (and
       (and
        (and
         (and
          (sized A&.)
          (let
           ((tmp%%$ 0))
           (let
            ((tmp%%$1 (%I j!)))
            (let
             ((tmp%%$2 (%I k1!)))
             (let
              ((tmp%%$3 (vstd!seq.Seq.len.? A&. A& s!)))
              (and
               (and
                (<= tmp%%$ tmp%%$1)
                (<= tmp%%$1 tmp%%$2)
               )
               (<= tmp%%$2 tmp%%$3)
         ))))))
         (let
          ((tmp%%$ 0))
          (let
           ((tmp%%$5 (%I j!)))
           (let
            ((tmp%%$6 (%I k2!)))
            (let
             ((tmp%%$7 (vstd!seq.Seq.len.? A&. A& s!)))
             (and
              (and
               (<= tmp%%$ tmp%%$5)
               (<= tmp%%$5 tmp%%$6)
              )
              (<= tmp%%$6 tmp%%$7)
        ))))))
        (let
         ((tmp%%$ 0))
         (let
          ((tmp%%$9 (%I i!)))
          (let
           ((tmp%%$10 (Sub (%I k1!) (%I j!))))
           (and
            (<= tmp%%$ tmp%%$9)
            (< tmp%%$9 tmp%%$10)
       )))))
       (let
        ((tmp%%$ 0))
        (let
         ((tmp%%$12 (%I i!)))
         (let
          ((tmp%%$13 (Sub (%I k2!) (%I j!))))
          (and
           (<= tmp%%$ tmp%%$12)
           (< tmp%%$12 tmp%%$13)
      )))))
      (= (vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.subrange.? A&. A& s! j! k1!) i!) (vstd!seq.Seq.index.?
        A&. A& (vstd!seq.Seq.subrange.? A&. A& s! j! k2!) i!
    ))))
    :pattern ((vstd!seq.Seq.index.? A&. A& (vstd!seq.Seq.subrange.? A&. A& s! j! k1!) i!)
     (vstd!seq.Seq.subrange.? A&. A& s! j! k2!)
    )
    :qid user_vstd__seq__lemma_seq_two_subranges_index_0
    :skolemid skolem_user_vstd__seq__lemma_seq_two_subranges_index_0
))))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.FnMut. F&. F& A&. A&)
    )
    (tr_bound%core!ops.function.FnOnce. $ (MUTREF F&. F&) A&. A&)
   )
   :pattern ((tr_bound%core!ops.function.FnOnce. $ (MUTREF F&. F&) A&. A&))
   :qid internal_core__ops__function__impls__impl&__4_trait_impl_definition
   :skolemid skolem_internal_core__ops__function__impls__impl&__4_trait_impl_definition
)))
(assert
 (=>
  (fuel_bool fuel%vstd!function.axiom_fn_mut_call_requires.)
  (forall ((Args&. Dcr) (Args& Type) (F&. Dcr) (F& Type) (f! Poly) (args! Poly)) (!
    (=>
     (and
      (has_type f! (MUTREF F&. F&))
      (has_type args! Args&)
     )
     (=>
      (and
       (and
        (and
         (and
          (sized Args&.)
          (sized F&.)
         )
         (tr_bound%core!marker.Tuple. Args&. Args&)
        )
        (tr_bound%core!ops.function.FnMut. F&. F& Args&. Args&)
       )
       (closure_req F& Args&. Args& (mut_ref_current% f!) args!)
      )
      (closure_req (MUTREF F&. F&) Args&. Args& f! args!)
    ))
    :pattern ((closure_req (MUTREF F&. F&) Args&. Args& f! args!))
    :qid user_vstd__function__axiom_fn_mut_call_requires_0
    :skolemid skolem_user_vstd__function__axiom_fn_mut_call_requires_0
))))
(assert
 (=>
  (fuel_bool fuel%vstd!function.axiom_fn_mut_call_ensures.)
  (forall ((Args&. Dcr) (Args& Type) (F&. Dcr) (F& Type) (f! Poly) (args! Poly) (output!
     Poly
    )
   ) (!
    (=>
     (and
      (has_type f! (MUTREF F&. F&))
      (has_type args! Args&)
      (has_type output! (proj%core!ops.function.FnOnce./Output F&. F& Args&. Args&))
     )
     (=>
      (and
       (and
        (and
         (and
          (sized Args&.)
          (sized F&.)
         )
         (tr_bound%core!marker.Tuple. Args&. Args&)
        )
        (tr_bound%core!ops.function.FnMut. F&. F& Args&. Args&)
       )
       (closure_ens (MUTREF F&. F&) Args&. Args& f! args! output!)
      )
      (closure_ens F& Args&. Args& (mut_ref_current% f!) args! output!)
    ))
    :pattern ((closure_ens (MUTREF F&. F&) Args&. Args& f! args! output!))
    :qid user_vstd__function__axiom_fn_mut_call_ensures_0
    :skolemid skolem_user_vstd__function__axiom_fn_mut_call_ensures_0
))))
(assert
 (tr_bound%core!marker.Tuple. $ TYPE%tuple%0.)
)
(assert
 (tr_bound%core!clone.Clone. $ TYPE%tuple%0.)
)
(assert
 (tr_bound%core!marker.Copy. $ TYPE%tuple%0.)
)
(assert
 (fuel_bool_default fuel%rlimit!above.)
)
(assert
 (=>
  (fuel_bool fuel%rlimit!above.)
  (forall ((s! Poly) (a! Poly)) (!
    (= (rlimit!above.? s! a!) (forall ((i$ Poly)) (!
       (=>
        (has_type i$ INT)
        (=>
         (let
          ((tmp%%$ 0))
          (let
           ((tmp%%$1 (%I i$)))
           (let
            ((tmp%%$2 (vstd!seq.Seq.len.? $ INT s!)))
            (and
             (<= tmp%%$ tmp%%$1)
             (< tmp%%$1 tmp%%$2)
         ))))
         (> (%I (vstd!seq.Seq.index.? $ INT s! i$)) (%I a!))
       ))
       :pattern ((vstd!seq.Seq.index.? $ INT s! i$))
       :qid user_rlimit__above_0
       :skolemid skolem_user_rlimit__above_0
    )))
    :pattern ((rlimit!above.? s! a!))
    :qid internal_rlimit!above.?_definition
    :skolemid skolem_internal_rlimit!above.?_definition
))))
(assert
 (forall ((T&. Dcr) (T& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized T&.)
     (sized A&.)
     (tr_bound%core!clone.Clone. T&. T&)
     (tr_bound%core!alloc.Allocator. A&. A&)
     (tr_bound%core!clone.Clone. A&. A&)
    )
    (tr_bound%core!clone.Clone. (BOX A&. A& T&.) T&)
   )
   :pattern ((tr_bound%core!clone.Clone. (BOX A&. A& T&.) T&))
   :qid internal_alloc__boxed__impl&__15_trait_impl_definition
   :skolemid skolem_internal_alloc__boxed__impl&__15_trait_impl_definition
)))
(assert
 (tr_bound%core!clone.Clone. $ (UINT 32))
)
(assert
 (tr_bound%core!clone.Clone. $ BOOL)
)
(assert
 (forall ((T&. Dcr) (T& Type)) (!
   (tr_bound%core!clone.Clone. (REF T&.) T&)
   :pattern ((tr_bound%core!clone.Clone. (REF T&.) T&))
   :qid internal_core__clone__impls__impl&__6_trait_impl_definition
   :skolemid skolem_internal_core__clone__impls__impl&__6_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type)) (!
   (=>
    (sized A&.)
    (tr_bound%core!clone.Clone. (GHOST A&.) A&)
   )
   :pattern ((tr_bound%core!clone.Clone. (GHOST A&.) A&))
   :qid internal_verus_builtin__impl&__7_trait_impl_definition
   :skolemid skolem_internal_verus_builtin__impl&__7_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Copy. A&. A&)
    )
    (tr_bound%core!clone.Clone. (TRACKED A&.) A&)
   )
   :pattern ((tr_bound%core!clone.Clone. (TRACKED A&.) A&))
   :qid internal_verus_builtin__impl&__9_trait_impl_definition
   :skolemid skolem_internal_verus_builtin__impl&__9_trait_impl_definition
)))
(assert
 (tr_bound%core!clone.Clone. $ NAT)
)
(assert
 (tr_bound%core!marker.Copy. $ (UINT 32))
)
(assert
 (tr_bound%core!marker.Copy. $ BOOL)
)
(assert
 (forall ((T&. Dcr) (T& Type)) (!
   (tr_bound%core!marker.Copy. (REF T&.) T&)
   :pattern ((tr_bound%core!marker.Copy. (REF T&.) T&))
   :qid internal_core__marker__impl&__4_trait_impl_definition
   :skolemid skolem_internal_core__marker__impl&__4_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type)) (!
   (=>
    (sized A&.)
    (tr_bound%core!marker.Copy. (GHOST A&.) A&)
   )
   :pattern ((tr_bound%core!marker.Copy. (GHOST A&.) A&))
   :qid internal_verus_builtin__impl&__8_trait_impl_definition
   :skolemid skolem_internal_verus_builtin__impl&__8_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Copy. A&. A&)
    )
    (tr_bound%core!marker.Copy. (TRACKED A&.) A&)
   )
   :pattern ((tr_bound%core!marker.Copy. (TRACKED A&.) A&))
   :qid internal_verus_builtin__impl&__10_trait_impl_definition
   :skolemid skolem_internal_verus_builtin__impl&__10_trait_impl_definition
)))
(assert
 (tr_bound%core!marker.Copy. $ NAT)
)
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.Fn. F&. F& A&. A&)
    )
    (tr_bound%core!ops.function.FnOnce. (REF F&.) F& A&. A&)
   )
   :pattern ((tr_bound%core!ops.function.FnOnce. (REF F&.) F& A&. A&))
   :qid internal_core__ops__function__impls__impl&__2_trait_impl_definition
   :skolemid skolem_internal_core__ops__function__impls__impl&__2_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.Fn. F&. F& A&. A&)
    )
    (tr_bound%core!ops.function.FnMut. (REF F&.) F& A&. A&)
   )
   :pattern ((tr_bound%core!ops.function.FnMut. (REF F&.) F& A&. A&))
   :qid internal_core__ops__function__impls__impl&__1_trait_impl_definition
   :skolemid skolem_internal_core__ops__function__impls__impl&__1_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.Fn. F&. F& A&. A&)
    )
    (tr_bound%core!ops.function.Fn. (REF F&.) F& A&. A&)
   )
   :pattern ((tr_bound%core!ops.function.Fn. (REF F&.) F& A&. A&))
   :qid internal_core__ops__function__impls__impl&__0_trait_impl_definition
   :skolemid skolem_internal_core__ops__function__impls__impl&__0_trait_impl_definition
)))
(assert
 (forall ((Args&. Dcr) (Args& Type) (F&. Dcr) (F& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized Args&.)
     (sized A&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
     (tr_bound%core!ops.function.FnOnce. F&. F& Args&. Args&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (tr_bound%core!ops.function.FnOnce. (BOX A&. A& F&.) F& Args&. Args&)
   )
   :pattern ((tr_bound%core!ops.function.FnOnce. (BOX A&. A& F&.) F& Args&. Args&))
   :qid internal_alloc__boxed__impl&__31_trait_impl_definition
   :skolemid skolem_internal_alloc__boxed__impl&__31_trait_impl_definition
)))
(assert
 (forall ((Args&. Dcr) (Args& Type) (F&. Dcr) (F& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized Args&.)
     (sized A&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
     (tr_bound%core!ops.function.FnMut. F&. F& Args&. Args&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (tr_bound%core!ops.function.FnMut. (BOX A&. A& F&.) F& Args&. Args&)
   )
   :pattern ((tr_bound%core!ops.function.FnMut. (BOX A&. A& F&.) F& Args&. Args&))
   :qid internal_alloc__boxed__impl&__32_trait_impl_definition
   :skolemid skolem_internal_alloc__boxed__impl&__32_trait_impl_definition
)))
(assert
 (forall ((Args&. Dcr) (Args& Type) (F&. Dcr) (F& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized Args&.)
     (sized A&.)
     (tr_bound%core!marker.Tuple. Args&. Args&)
     (tr_bound%core!ops.function.Fn. F&. F& Args&. Args&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (tr_bound%core!ops.function.Fn. (BOX A&. A& F&.) F& Args&. Args&)
   )
   :pattern ((tr_bound%core!ops.function.Fn. (BOX A&. A& F&.) F& Args&. Args&))
   :qid internal_alloc__boxed__impl&__33_trait_impl_definition
   :skolemid skolem_internal_alloc__boxed__impl&__33_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type) (F&. Dcr) (F& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!marker.Tuple. A&. A&)
     (tr_bound%core!ops.function.FnMut. F&. F& A&. A&)
    )
    (tr_bound%core!ops.function.FnMut. $ (MUTREF F&. F&) A&. A&)
   )
   :pattern ((tr_bound%core!ops.function.FnMut. $ (MUTREF F&. F&) A&. A&))
   :qid internal_core__ops__function__impls__impl&__3_trait_impl_definition
   :skolemid skolem_internal_core__ops__function__impls__impl&__3_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type)) (!
   (=>
    (tr_bound%core!alloc.Allocator. A&. A&)
    (tr_bound%core!alloc.Allocator. (REF A&.) A&)
   )
   :pattern ((tr_bound%core!alloc.Allocator. (REF A&.) A&))
   :qid internal_core__alloc__impl&__2_trait_impl_definition
   :skolemid skolem_internal_core__alloc__impl&__2_trait_impl_definition
)))
(assert
 (forall ((A&. Dcr) (A& Type)) (!
   (=>
    (tr_bound%core!alloc.Allocator. A&. A&)
    (tr_bound%core!alloc.Allocator. $ (MUTREF A&. A&))
   )
   :pattern ((tr_bound%core!alloc.Allocator. $ (MUTREF A&. A&)))
   :qid internal_core__alloc__impl&__3_trait_impl_definition
   :skolemid skolem_internal_core__alloc__impl&__3_trait_impl_definition
)))
(assert
 (forall ((T&. Dcr) (T& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!alloc.Allocator. T&. T&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (tr_bound%core!alloc.Allocator. (BOX A&. A& T&.) T&)
   )
   :pattern ((tr_bound%core!alloc.Allocator. (BOX A&. A& T&.) T&))
   :qid internal_alloc__boxed__impl&__49_trait_impl_definition
   :skolemid skolem_internal_alloc__boxed__impl&__49_trait_impl_definition
)))
(assert
 (forall ((T&. Dcr) (T& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!alloc.Allocator. T&. T&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (tr_bound%core!alloc.Allocator. (RC A&. A& T&.) T&)
   )
   :pattern ((tr_bound%core!alloc.Allocator. (RC A&. A& T&.) T&))
   :qid internal_alloc__rc__impl&__116_trait_impl_definition
   :skolemid skolem_internal_alloc__rc__impl&__116_trait_impl_definition
)))
(assert
 (forall ((T&. Dcr) (T& Type) (A&. Dcr) (A& Type)) (!
   (=>
    (and
     (sized A&.)
     (tr_bound%core!alloc.Allocator. T&. T&)
     (tr_bound%core!alloc.Allocator. A&. A&)
    )
    (tr_bound%core!alloc.Allocator. (ARC A&. A& T&.) T&)
   )
   :pattern ((tr_bound%core!alloc.Allocator. (ARC A&. A& T&.) T&))
   :qid internal_alloc__sync__impl&__118_trait_impl_definition
   :skolemid skolem_internal_alloc__sync__impl&__118_trait_impl_definition
)))
(assert
 (tr_bound%verus_builtin!Integer. $ (UINT 32))
)
(assert
 (tr_bound%verus_builtin!Integer. $ NAT)
)
(declare-fun req%rlimit!double. (Int) Bool)
(declare-const %%global_location_label%%4 Bool)
(assert
 (forall ((a! Int)) (!
   (= (req%rlimit!double. a!) (=>
     %%global_location_label%%4
     (< a! 100)
   ))
   :pattern ((req%rlimit!double. a!))
   :qid internal_req__rlimit!double._definition
   :skolemid skolem_internal_req__rlimit!double._definition
)))
(declare-fun ens%rlimit!double. (Int Int) Bool)
(assert
 (forall ((a! Int) (r! Int)) (!
   (= (ens%rlimit!double. a! r!) (and
     (uInv 32 r!)
     (= r! (Mul a! 2))
   ))
   :pattern ((ens%rlimit!double. a! r!))
   :qid internal_ens__rlimit!double._definition
   :skolemid skolem_internal_ens__rlimit!double._definition
)))
(push)
(get-info :all-statistics)

(echo "<<DONE>>")
(declare-const r! Int)
(declare-const a! Int)
(assert
 fuel_defaults
)
(assert
 (uInv 32 a!)
)
(assert
 (< a! 100)
)
(declare-const %%location_label%%0 Bool)
(declare-const %%location_label%%1 Bool)
(assert
 (not (and
   (=>
    %%location_label%%0
    (uInv 32 (Add a! a!))
   )
   (=>
    (uInv 32 (Add a! a!))
    (=>
     (= r! (uClip 32 (Add a! a!)))
     (=>
      %%location_label%%1
      (= r! (Mul a! 2))
))))))
(get-info :all-statistics)

(echo "<<DONE>>")
(get-info :version)

(echo "<<DONE>>")
(set-option :rlimit 30000000)
(check-sat)

(echo "<<DONE>>")
(set-option :rlimit 0)
(get-info :all-statistics)

(echo "<<DONE>>")
(pop)
(declare-fun req%rlimit!too_expensive. (vstd!seq.Seq<int.>. Int Int) Bool)
(declare-const %%global_location_label%%5 Bool)
(declare-const %%global_location_label%%6 Bool)
(declare-const %%global_location_label%%7 Bool)
(assert
 (forall ((s! vstd!seq.Seq<int.>.) (a! Int) (b! Int)) (!
   (= (req%rlimit!too_expensive. s! a! b!) (and
     (=>
      %%global_location_label%%5
      (> (vstd!seq.Seq.len.? $ INT (Poly%vstd!seq.Seq<int.>. s!)) 10)
     )
     (=>
      %%global_location_label%%6
      (rlimit!above.? (Poly%vstd!seq.Seq<int.>. s!) (I a!))
     )
     (=>
      %%global_location_label%%7
      (> a! b!)
   )))
   :pattern ((req%rlimit!too_expensive. s! a! b!))
   :qid internal_req__rlimit!too_expensive._definition
   :skolemid skolem_internal_req__rlimit!too_expensive._definition
)))
(declare-fun ens%rlimit!too_expensive. (vstd!seq.Seq<int.>. Int Int) Bool)
(assert
 (forall ((s! vstd!seq.Seq<int.>.) (a! Int) (b! Int)) (!
   (= (ens%rlimit!too_expensive. s! a! b!) (and
     (= (vstd!seq.Seq.len.? $ INT (vstd!seq.Seq.push.? $ INT (vstd!seq.Seq.subrange.? $ INT
         (Poly%vstd!seq.Seq<int.>. s!) (I 2) (I 8)
        ) (I (Add a! 1))
       )
      ) 7
     )
     (rlimit!above.? (vstd!seq.Seq.subrange.? $ INT (Poly%vstd!seq.Seq<int.>. s!) (I 2)
       (I 8)
      ) (I b!)
   )))
   :pattern ((ens%rlimit!too_expensive. s! a! b!))
   :qid internal_ens__rlimit!too_expensive._definition
   :skolemid skolem_internal_ens__rlimit!too_expensive._definition
)))
(push)
(get-info :all-statistics)

(echo "<<DONE>>")
(declare-const s! vstd!seq.Seq<int.>.)
(declare-const a! Int)
(declare-const b! Int)
(assert
 fuel_defaults
)
(assert
 (> (vstd!seq.Seq.len.? $ INT (Poly%vstd!seq.Seq<int.>. s!)) 10)
)
(assert
 (rlimit!above.? (Poly%vstd!seq.Seq<int.>. s!) (I a!))
)
(assert
 (> a! b!)
)
(declare-const %%location_label%%0 Bool)
(declare-const %%location_label%%1 Bool)
(assert
 (not (and
   (=>
    %%location_label%%0
    (= (vstd!seq.Seq.len.? $ INT (vstd!seq.Seq.push.? $ INT (vstd!seq.Seq.subrange.? $ INT
        (Poly%vstd!seq.Seq<int.>. s!) (I 2) (I 8)
       ) (I (Add a! 1))
      )
     ) 7
   ))
   (=>
    %%location_label%%1
    (rlimit!above.? (vstd!seq.Seq.subrange.? $ INT (Poly%vstd!seq.Seq<int.>. s!) (I 2)
      (I 8)
     ) (I b!)
)))))
(get-info :all-statistics)

(echo "<<DONE>>")
(get-info :version)

(echo "<<DONE>>")
(set-option :rlimit 300)
(check-sat)

(echo "<<DONE>>")
(set-option :rlimit 0)
(get-info :reason-unknown)

(echo "<<DONE>>")
(get-info :all-statistics)

(echo "<<DONE>>")
(pop)
(declare-fun req%rlimit!cheap. (vstd!seq.Seq<int.>. Int) Bool)
(declare-const %%global_location_label%%8 Bool)
(declare-const %%global_location_label%%9 Bool)
(assert
 (forall ((s! vstd!seq.Seq<int.>.) (a! Int)) (!
   (= (req%rlimit!cheap. s! a!) (and
     (=>
      %%global_location_label%%8
      (rlimit!above.? (Poly%vstd!seq.Seq<int.>. s!) (I a!))
     )
     (=>
      %%global_location_label%%9
      (> (vstd!seq.Seq.len.? $ INT (Poly%vstd!seq.Seq<int.>. s!)) 0)
   )))
   :pattern ((req%rlimit!cheap. s! a!))
   :qid internal_req__rlimit!cheap._definition
   :skolemid skolem_internal_req__rlimit!cheap._definition
)))
(declare-fun ens%rlimit!cheap. (vstd!seq.Seq<int.>. Int) Bool)
(assert
 (forall ((s! vstd!seq.Seq<int.>.) (a! Int)) (!
   (= (ens%rlimit!cheap. s! a!) (> (%I (vstd!seq.Seq.index.? $ INT (Poly%vstd!seq.Seq<int.>.
        s!
       ) (I 0)
      )
     ) a!
   ))
   :pattern ((ens%rlimit!cheap. s! a!))
   :qid internal_ens__rlimit!cheap._definition
   :skolemid skolem_internal_ens__rlimit!cheap._definition
)))
(push)
(get-info :all-statistics)

(echo "<<DONE>>")
(declare-const s! vstd!seq.Seq<int.>.)
(declare-const a! Int)
(assert
 fuel_defaults
)
(assert
 (rlimit!above.? (Poly%vstd!seq.Seq<int.>. s!) (I a!))
)
(assert
 (> (vstd!seq.Seq.len.? $ INT (Poly%vstd!seq.Seq<int.>. s!)) 0)
)
(declare-const %%location_label%%0 Bool)
(assert
 (not (=>
   %%location_label%%0
   (> (%I (vstd!seq.Seq.index.? $ INT (Poly%vstd!seq.Seq<int.>. s!) (I 0))) a!)
)))
(get-info :all-statistics)

(echo "<<DONE>>")
(get-info :version)

(echo "<<DONE>>")
(set-option :rlimit 30000000)
(check-sat)

(echo "<<DONE>>")
(set-option :rlimit 0)
(get-info :all-statistics)

(echo "<<DONE>>")
(pop)
(push)
(get-info :all-statistics)

(echo "<<DONE>>")
(declare-const no%param Int)
(assert
 fuel_defaults
)
(assert
 (not true)
)
(get-info :all-statistics)

(echo "<<DONE>>")
(get-info :version)

(echo "<<DONE>>")
(set-option :rlimit 30000000)
(check-sat)

(echo "<<DONE>>")
(set-option :rlimit 0)
(get-info :all-statistics)

(echo "<<DONE>>")
