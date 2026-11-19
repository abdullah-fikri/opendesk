# Fonts

The redesign's typefaces, embedded into the app by `app_window.slint`:

- **Inter** (Regular, Medium, SemiBold, Bold): text. https://github.com/rsms/inter
- **JetBrains Mono** (Regular, Medium): device IDs. https://github.com/JetBrains/JetBrainsMono

Both are licensed under the SIL Open Font License 1.1 (https://openfontlicense.org). These files are static
instances of the variable Latin subsets used by the design export (`OpenDesk Desktop Redesign`), made with
fontTools `varLib.instancer`. Characters outside the subset fall back to the system fonts.
