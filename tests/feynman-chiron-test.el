;;; feynman-chiron-test.el --- ERT tests for feynman-chiron.el -*- lexical-binding: t; -*-

(require 'ert)
(require 'feynman-chiron)

(defmacro with-two-temp-buffers (b1 b2 &rest body)
  "Evaluate BODY with B1 and B2 bound to fresh temp buffers, cleaned up after."
  (declare (indent 2))
  `(let ((,b1 (generate-new-buffer "fctest-1"))
         (,b2 (generate-new-buffer "fctest-2")))
     (unwind-protect
         (progn ,@body)
       (kill-buffer ,b1)
       (kill-buffer ,b2))))

(ert-deftest test-backend-process-is-buffer-local ()
  "feynman-chiron-backend-process must be buffer-local so each org file
gets its own backend process reference."
  (with-two-temp-buffers b1 b2
    (with-current-buffer b1 (setq feynman-chiron-backend-process 'proc-a))
    (with-current-buffer b2 (setq feynman-chiron-backend-process 'proc-b))
    (should (eq (buffer-local-value 'feynman-chiron-backend-process b1) 'proc-a))
    (should (eq (buffer-local-value 'feynman-chiron-backend-process b2) 'proc-b))))

(ert-deftest test-state-is-buffer-local ()
  "feynman-chiron-state must be buffer-local so each org file tracks
its own learning state independently."
  (with-two-temp-buffers b1 b2
    (with-current-buffer b1 (setq feynman-chiron-state '(:concept "algebra")))
    (with-current-buffer b2 (setq feynman-chiron-state '(:concept "topology")))
    (should (equal (buffer-local-value 'feynman-chiron-state b1) '(:concept "algebra")))
    (should (equal (buffer-local-value 'feynman-chiron-state b2) '(:concept "topology")))))

(ert-deftest test-prompt-marker-is-buffer-local ()
  "feynman-chiron-prompt-marker must be buffer-local so each buffer's
prompt position is tracked independently."
  (with-two-temp-buffers b1 b2
    (with-current-buffer b1 (setq feynman-chiron-prompt-marker 'marker-a))
    (with-current-buffer b2 (setq feynman-chiron-prompt-marker 'marker-b))
    (should (eq (buffer-local-value 'feynman-chiron-prompt-marker b1) 'marker-a))
    (should (eq (buffer-local-value 'feynman-chiron-prompt-marker b2) 'marker-b))))

;;; feynman-chiron-test.el ends here
