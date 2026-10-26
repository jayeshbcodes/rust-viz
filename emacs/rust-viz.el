;;; rust-viz.el --- Inline Rust ownership & memory visualization -*- lexical-binding: t; -*-

;; Copyright (C) 2026 jayeshbcodes

;; Author: jayeshbcodes
;; URL: https://github.com/jayeshbcodes/rust-viz
;; Version: 0.1.0
;; Package-Requires: ((emacs "27.1"))
;; Keywords: languages, tools
;; SPDX-License-Identifier: MIT OR Apache-2.0

;;; Commentary:

;; Runs the Rust program in the current buffer through `rust-viz' and
;; shows what happens to memory right in the source:
;;
;;   let s = String::from("hello");  ⟵ s = String "hello" [ptr → #0, len 5, cap 5] · alloc #0
;;   takes_ownership(s);             ⟵ moved s · → takes_ownership()
;;
;; Commands:
;;   `rust-viz-run'    annotate every executed line with its final effect,
;;                     and show the stack/heap for the line at point in a
;;                     side window (it follows the cursor).
;;   `rust-viz-step'   step through execution: n/p next/previous, g/G
;;                     first/last, q quit.  The current line is
;;                     highlighted, annotations replay up to that step, and
;;                     a side window shows the full stack, heap and events.
;;   `rust-viz-clear'  remove annotations.
;;   `rust-viz-mode'   minor mode: C-c C-v r/s/c for the above, and
;;                     re-annotate on save.
;;
;; Needs the `rust-viz' binary (`cargo install --path .' in the repo).
;; The buffer's text is piped in, so it needn't be saved first.

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'seq)
(require 'subr-x)

(defgroup rust-viz nil
  "Inline ownership/memory visualization for Rust."
  :group 'tools
  :prefix "rust-viz-")

(defcustom rust-viz-executable "rust-viz"
  "Path to the rust-viz binary."
  :type 'string)

(defcustom rust-viz-annotation-width 110
  "Inline annotations longer than this many characters are truncated.
Error annotations are always shown in full."
  :type 'integer)

(defcustom rust-viz-annotate-on-save t
  "When `rust-viz-mode' is on, re-run and re-annotate after each save."
  :type 'boolean)

(defcustom rust-viz-detail-window-position 'bottom
  "Where the stack/heap window opens: a full-width split at the `bottom',
or a split on the `right'.  It's an ordinary window, so you can resize it
by dragging its border with the mouse; rust-viz only sets its size when
it first opens."
  :type '(choice (const bottom) (const right)))

(defcustom rust-viz-detail-window-size 0.35
  "Initial size of the stack/heap window, as a fraction of the frame."
  :type 'number)

(defface rust-viz-annotation-face
  '((t :inherit font-lock-comment-face :slant italic))
  "Face for inline annotations.")

(defface rust-viz-error-face
  '((t :inherit error))
  "Face for the annotation on the line where the program stopped with an error.")

(defface rust-viz-current-line-face
  '((t :inherit highlight :extend t))
  "Face for the line of the current step while stepping.")

(defvar-local rust-viz--trace nil
  "Parsed JSON trace of the last run, an alist.")

(defvar-local rust-viz--index nil
  "Index of the current step while stepping.")

(defvar-local rust-viz--line-overlay nil)

(defvar-local rust-viz--was-read-only nil)

(defvar-local rust-viz--follow-line nil
  "Line whose state the side window currently shows, after `rust-viz-run'.")

(defconst rust-viz--detail-buffer "*rust-viz*")

;;;; Running

(defun rust-viz--executable ()
  (or (executable-find rust-viz-executable)
      (user-error "Can't find `%s' — install it with `cargo install --path .' in the rust-viz repo, or set `rust-viz-executable'"
                  rust-viz-executable)))

(defun rust-viz--parse (json-text)
  (json-parse-string json-text :object-type 'alist :array-type 'array
                     :null-object nil :false-object nil))

(defun rust-viz--run-process ()
  "Pipe the buffer through `rust-viz --json -' and return the parsed trace."
  (let ((exe (rust-viz--executable))
        (src (buffer-substring-no-properties (point-min) (point-max))))
    (with-temp-buffer
      (let* ((coding-system-for-write 'utf-8)
             (coding-system-for-read 'utf-8)
             (status (call-process-region src nil exe nil t nil "--json" "-")))
        (unless (eq status 0)
          (error "rust-viz failed (exit %s): %s" status (string-trim (buffer-string))))
        (rust-viz--parse (buffer-string))))))

(defun rust-viz--steps () (alist-get 'steps rust-viz--trace))

(defun rust-viz--step (i) (aref (rust-viz--steps) i))

;;;; Annotations

(defun rust-viz--label (step)
  "Inline text for STEP: what happened (moves, calls, allocations), then
the resulting values of any bindings it set."
  (let ((changed (mapcar (lambda (c) (format "%s = %s" (alist-get 'name c) (alist-get 'value c)))
                         (alist-get 'changed step)))
        (short (append (alist-get 'short step) nil)))
    (string-join (append short changed) " · ")))

(defun rust-viz--visit (line upto)
  "Steps making up the latest execution of LINE at or before step UPTO.
A call on LINE runs the callee's lines in between; those are skipped by
call depth so the call and its completion still read as one visit.
Returns (STEPS . VISIT-COUNT), or nil if LINE hasn't run yet."
  (let ((last nil))
    (cl-loop for i downfrom upto to 0
             when (= (alist-get 'line (rust-viz--step i)) line)
             return (setq last i))
    (when last
      (let* ((depth (alist-get 'depth (rust-viz--step last)))
             (steps (list (rust-viz--step last)))
             (count 0))
        (cl-loop for i downfrom (1- last) to 0
                 for s = (rust-viz--step i)
                 do (cond ((= (alist-get 'line s) line) (push s steps))
                          ((> (alist-get 'depth s) depth))
                          (t (cl-return))))
        (dotimes (i (1+ upto))
          (let ((s (rust-viz--step i)))
            (when (and (= (alist-get 'line s) line) (= (alist-get 'depth s) depth))
              (setq count (1+ count)))))
        (cons steps count)))))

(defun rust-viz--error-line ()
  "Line the program stopped on with an error, if it did."
  (when (alist-get 'error rust-viz--trace)
    (let ((steps (rust-viz--steps)))
      (alist-get 'line (aref steps (1- (length steps)))))))

(defun rust-viz-clear ()
  "Remove rust-viz annotations from the buffer."
  (interactive)
  (remove-overlays (point-min) (point-max) 'rust-viz t)
  (remove-hook 'post-command-hook #'rust-viz--follow t)
  (setq rust-viz--line-overlay nil
        rust-viz--follow-line nil))

(defun rust-viz--annotate (upto)
  "Annotate each line with what its latest execution up to step UPTO did."
  (remove-overlays (point-min) (point-max) 'rust-viz-annotation t)
  (let ((lines (delete-dups (mapcar (lambda (s) (alist-get 'line s))
                                    (seq-take (append (rust-viz--steps) nil) (1+ upto)))))
        (err-line (and (= upto (1- (length (rust-viz--steps)))) (rust-viz--error-line))))
    (save-excursion
      (dolist (line lines)
        (when (> line 0)
          (let* ((visit (rust-viz--visit line upto))
                 (text (string-join (seq-remove #'string-empty-p (mapcar #'rust-viz--label (car visit))) " · "))
                 (err (and err-line (= line err-line))))
            (when err
              (setq text (concat "✗ " (alist-get 'error rust-viz--trace))))
            (when (> (cdr visit) 1)
              (setq text (format "%s  (×%d)" text (cdr visit))))
            (unless (string-empty-p text)
              (goto-char (point-min))
              (forward-line (1- line))
              (let* ((eol (line-end-position))
                     (ov (make-overlay eol eol nil t nil)))
                (overlay-put ov 'rust-viz t)
                (overlay-put ov 'rust-viz-annotation t)
                (overlay-put ov 'after-string
                             (propertize (concat "  ⟵ " (if err text (truncate-string-to-width text rust-viz-annotation-width nil nil "…")))
                                         'face (if err 'rust-viz-error-face 'rust-viz-annotation-face)
                                         'cursor t))))))))))

;;;###autoload
(defun rust-viz-run ()
  "Run the buffer's program and annotate each executed line inline."
  (interactive)
  (rust-viz-clear)
  (setq rust-viz--trace (rust-viz--run-process))
  (let ((n (length (rust-viz--steps))))
    (if (= n 0)
        (message "rust-viz: %s" (or (alist-get 'error rust-viz--trace) "nothing ran"))
      (rust-viz--annotate (1- n))
      (add-hook 'post-command-hook #'rust-viz--follow nil t)
      (rust-viz--follow t)
      (if-let* ((err (alist-get 'error rust-viz--trace)))
          (message "rust-viz: stopped — %s" err)
        (message "rust-viz: %d steps. Output: %s" n
                 (truncate-string-to-width (string-trim (alist-get 'output rust-viz--trace)) 80 nil nil "…"))))))

;;;; Following the cursor

(defvar rust-viz-stepping-mode) ; defined below

(defun rust-viz--last-step-on-line (line)
  (cl-loop for i downfrom (1- (length (rust-viz--steps))) to 0
           when (= (alist-get 'line (rust-viz--step i)) line) return i))

(defun rust-viz--follow (&optional show)
  "Update the side window to the state after the line at point last ran.
With SHOW, also display the window (otherwise it's only refreshed if
visible, so closing it isn't undone by moving the cursor)."
  (when (and rust-viz--trace (not rust-viz-stepping-mode))
    (let ((line (line-number-at-pos)))
      (unless (and (eql line rust-viz--follow-line) (not show))
        (setq rust-viz--follow-line line)
        (let ((i (rust-viz--last-step-on-line line))
              (n (length (rust-viz--steps))))
          (rust-viz--fill-detail
           (if i
               (concat (format "State right after line %d last ran (step %d of %d).\n" line (1+ i) n)
                       "Move the cursor to inspect another line · C-c C-v s to step through.\n\n"
                       (rust-viz--detail-text (rust-viz--step i) i n))
             (format "Line %d didn't run (or changed nothing).\nPut the cursor on an annotated line to see the stack and heap after it ran.\n" line)))
          (when show
            (rust-viz--display-detail)))))))

;;;; Stepping

(defvar rust-viz-stepping-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "n") #'rust-viz-next)
    (define-key map (kbd "SPC") #'rust-viz-next)
    (define-key map (kbd "p") #'rust-viz-previous)
    (define-key map (kbd "DEL") #'rust-viz-previous)
    (define-key map (kbd "g") #'rust-viz-first)
    (define-key map (kbd "G") #'rust-viz-last)
    (define-key map (kbd "q") #'rust-viz-quit)
    map)
  "Keys active while stepping through a trace.")

(define-minor-mode rust-viz-stepping-mode
  "Stepping through a rust-viz trace. \\{rust-viz-stepping-mode-map}"
  :lighter " viz-step"
  :keymap rust-viz-stepping-mode-map
  (if rust-viz-stepping-mode
      (progn (setq rust-viz--was-read-only buffer-read-only)
             (setq buffer-read-only t))
    (setq buffer-read-only rust-viz--was-read-only)))

;;;###autoload
(defun rust-viz-step ()
  "Run the buffer's program and step through it."
  (interactive)
  (rust-viz-clear)
  (setq rust-viz--trace (rust-viz--run-process))
  (when (= (length (rust-viz--steps)) 0)
    (user-error "rust-viz: %s" (or (alist-get 'error rust-viz--trace) "nothing ran")))
  ;; Already stepping (pressed again): don't re-enter the mode, which would
  ;; record the buffer's current read-only state as the one to restore.
  (unless rust-viz-stepping-mode
    (rust-viz-stepping-mode 1))
  (rust-viz--goto 0))

(defun rust-viz--goto (i)
  (let* ((n (length (rust-viz--steps)))
         (i (max 0 (min i (1- n))))
         (step (rust-viz--step i))
         (line (alist-get 'line step)))
    (setq rust-viz--index i)
    (rust-viz--annotate i)
    (when (> line 0)
      (goto-char (point-min))
      (forward-line (1- line))
      (unless rust-viz--line-overlay
        (setq rust-viz--line-overlay (make-overlay 1 1))
        (overlay-put rust-viz--line-overlay 'rust-viz t)
        (overlay-put rust-viz--line-overlay 'face 'rust-viz-current-line-face))
      (move-overlay rust-viz--line-overlay (line-beginning-position) (1+ (line-end-position))))
    (rust-viz--fill-detail (rust-viz--detail-text step i n))
    (rust-viz--display-detail)
    (message "rust-viz: step %d/%d%s   n/p step · g/G first/last · q quit" (1+ i) n
             (if (= line 0) " (end of program)" ""))))

(defun rust-viz--detail-text (step i n)
  "Full stack/heap/events text for STEP (index I of N), plus output so far."
  (let ((output (substring (alist-get 'output rust-viz--trace) 0
                           (min (alist-get 'output_len step)
                                (length (alist-get 'output rust-viz--trace)))))
        (err (and (= i (1- n)) (alist-get 'error rust-viz--trace))))
    (concat (alist-get 'detail step)
            (if err (concat "\n" (propertize (concat "✗ " err) 'face 'rust-viz-error-face) "\n") "")
            "\n program output so far\n"
            (if (string-empty-p output) "   (none yet)\n" output))))

(defun rust-viz--fill-detail (text)
  (with-current-buffer (get-buffer-create rust-viz--detail-buffer)
    (let ((inhibit-read-only t))
      (erase-buffer)
      (insert text)
      (goto-char (point-min))
      ;; Make the section headings stand out.
      (while (re-search-forward "^ \\(what happened\\|stack\\|heap\\|program output so far\\)$" nil t)
        (put-text-property (match-beginning 1) (match-end 1) 'face 'bold))
      (goto-char (point-min))
      (unless (derived-mode-p 'special-mode) (special-mode)))))

(defun rust-viz--display-detail ()
  "Show the stack/heap window, sizing it only when it first opens — so a
size you've dragged it to sticks while stepping."
  (unless (get-buffer-window rust-viz--detail-buffer)
    (display-buffer rust-viz--detail-buffer
                    (if (eq rust-viz-detail-window-position 'right)
                        `((display-buffer-in-direction)
                          (direction . right) (window . root)
                          (window-width . ,rust-viz-detail-window-size))
                      `((display-buffer-at-bottom)
                        (window-height . ,rust-viz-detail-window-size))))))

(defun rust-viz-next ()
  "Go to the next step."
  (interactive)
  (rust-viz--goto (1+ rust-viz--index)))

(defun rust-viz-previous ()
  "Go to the previous step."
  (interactive)
  (rust-viz--goto (1- rust-viz--index)))

(defun rust-viz-first ()
  "Go to the first step."
  (interactive)
  (rust-viz--goto 0))

(defun rust-viz-last ()
  "Go to the last step."
  (interactive)
  (rust-viz--goto (1- (length (rust-viz--steps)))))

(defun rust-viz-quit ()
  "Stop stepping and remove annotations."
  (interactive)
  (rust-viz-stepping-mode -1)
  (rust-viz-clear)
  (when-let* ((win (get-buffer-window rust-viz--detail-buffer)))
    (delete-window win)))

;;;; Minor mode

(defvar rust-viz-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-v r") #'rust-viz-run)
    (define-key map (kbd "C-c C-v s") #'rust-viz-step)
    (define-key map (kbd "C-c C-v c") #'rust-viz-clear)
    map)
  "Keymap for `rust-viz-mode'.")

(defun rust-viz--after-save ()
  (when (and rust-viz-annotate-on-save (not rust-viz-stepping-mode))
    (condition-case e
        (rust-viz-run)
      (error (message "rust-viz: %s" (error-message-string e))))))

;;;###autoload
(define-minor-mode rust-viz-mode
  "Inline memory/ownership annotations from rust-viz.
\\{rust-viz-mode-map}"
  :lighter " viz"
  :keymap rust-viz-mode-map
  (if rust-viz-mode
      (add-hook 'after-save-hook #'rust-viz--after-save nil t)
    (remove-hook 'after-save-hook #'rust-viz--after-save t)
    (rust-viz-clear)))

(provide 'rust-viz)
;;; rust-viz.el ends here
