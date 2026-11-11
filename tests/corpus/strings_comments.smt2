; leading comment (with a paren
(echo "plain")  (echo "two ""quoted"" words") (echo "")
(echo "semi;colon (paren | bar")
(declare-const |weird ) name ;| Int) ; trailing comment
(assert
   (>
      |weird ) name ;|   ; inner comment )
      3))
(check-sat-assuming ((> |weird ) name ;| 4)))
(echo "done")
