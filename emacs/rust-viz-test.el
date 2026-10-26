;;; rust-viz-test.el --- ERT tests for rust-viz.el -*- lexical-binding: t; -*-

;; Run from the repo root (after `cargo build`):
;;   emacs --batch -L emacs -l emacs/rust-viz-test.el -f ert-run-tests-batch-and-exit

;;; Code:

(require 'ert)
(require 'rust-viz)

(defconst rust-viz-test--root
  (file-name-directory (directory-file-name (file-name-directory (or load-file-name buffer-file-name)))))

(setq rust-viz-executable (expand-file-name "target/debug/rust-viz" rust-viz-test--root))

(defmacro rust-viz-test--with-sample (name &rest body)
  "Run BODY in a buffer visiting samples/NAME."
  (declare (indent 1))
  `(with-temp-buffer
     (insert-file-contents (expand-file-name (concat "samples/" ,name) rust-viz-test--root))
     ,@body))

(defun rust-viz-test--annotation (line)
  "The inline annotation text at the end of LINE, or nil."
  (save-excursion
    (goto-char (point-min))
    (forward-line (1- line))
    (let ((eol (line-end-position)))
      (cl-some (lambda (ov) (and (overlay-get ov 'rust-viz-annotation)
                                 (substring-no-properties (overlay-get ov 'after-string))))
               (overlays-in eol eol)))))

(ert-deftest rust-viz-run-annotates-lines ()
  (rust-viz-test--with-sample "ownership.rs"
    (rust-viz-run)
    ;; let s = String::from("hello");
    (should (string-match-p "alloc #0 · s = String \"hello\"  \\[ptr → #0, len 5, cap 5\\]" (rust-viz-test--annotation 3)))
    ;; takes_ownership(s); — the move, the call, and the free inside it.
    (should (string-match-p "moved s · → takes_ownership()" (rust-viz-test--annotation 4)))
    (should (string-match-p "free #0" (rust-viz-test--annotation 19)))))

(ert-deftest rust-viz-run-shows-nll-borrow-end ()
  (rust-viz-test--with-sample "borrowing.rs"
    (rust-viz-run)
    ;; println!("{r1} and {r2}"); is the last use of r1/r2.
    (should (string-match-p "r1's borrow ends (last use)" (rust-viz-test--annotation 12)))))

(ert-deftest rust-viz-loop-lines-show-visit-count ()
  (rust-viz-test--with-sample "control_flow.rs"
    (rust-viz-run)
    ;; `n -= 1;` runs three times.
    (should (string-match-p "(×3)" (rust-viz-test--annotation 26)))))

(ert-deftest rust-viz-error-is-annotated-on-its-line ()
  (with-temp-buffer
    (insert "fn main() {\n    let s = String::from(\"a\");\n    let t = s;\n    println!(\"{s}\");\n}\n")
    (rust-viz-run)
    (should (string-match-p "✗ .*E0382" (rust-viz-test--annotation 4)))))

(ert-deftest rust-viz-step-replays-and-shows-detail ()
  (rust-viz-test--with-sample "ownership.rs"
    (rust-viz-step)
    (unwind-protect
        (progn
          (should rust-viz-stepping-mode)
          (should buffer-read-only)
          (should (= rust-viz--index 0))
          (should (= (line-number-at-pos) 2))
          ;; Nothing past the current step is annotated yet.
          (should-not (rust-viz-test--annotation 3))
          (rust-viz-next)
          (should (= (line-number-at-pos) 3))
          (should (rust-viz-test--annotation 3))
          (should (string-match-p " stack\n   fn main\n     s = String.*\n heap\n   #0: String buffer \"hello\".*← owned by s"
                                  (with-current-buffer "*rust-viz*" (buffer-string))))
          (rust-viz-last)
          (should (string-match-p "program output so far\nhello"
                                  (with-current-buffer "*rust-viz*" (buffer-string))))
          (rust-viz-previous)
          (should (= rust-viz--index (- (length (rust-viz--steps)) 2))))
      (rust-viz-quit))
    (should-not rust-viz-stepping-mode)
    (should-not buffer-read-only)
    (should-not (rust-viz-test--annotation 3))))

(ert-deftest rust-viz-works-on-unsaved-snippets ()
  (with-temp-buffer
    (insert "let mut v = Vec::new();\nv.push(1);\n")
    (rust-viz-run)
    (should (string-match-p "alloc #0" (rust-viz-test--annotation 2)))))

(ert-deftest rust-viz-run-side-window-follows-cursor ()
  (rust-viz-test--with-sample "ownership.rs"
    (rust-viz-run)
    (let ((detail (lambda () (with-current-buffer "*rust-viz*" (buffer-string)))))
      ;; After `let s = String::from("hello");` the heap holds s's buffer.
      (goto-char (point-min)) (forward-line 2)
      (rust-viz--follow)
      (should (string-match-p "right after line 3" (funcall detail)))
      (should (string-match-p "#0: String buffer \"hello\".*← owned by s" (funcall detail)))
      ;; After `takes_ownership(s);` s is moved and its buffer freed.
      (forward-line 1)
      (rust-viz--follow)
      (should (string-match-p "s = <moved out>" (funcall detail)))
      (should (string-match-p "#0: freed" (funcall detail)))
      ;; A comment line didn't run.
      (goto-char (point-min))
      (rust-viz--follow)
      (should (string-match-p "didn't run" (funcall detail))))
    (rust-viz-clear)
    (should-not (memq #'rust-viz--follow post-command-hook))))

;;; rust-viz-test.el ends here
