using System;
using System.Drawing;
using System.IO;
using System.Reflection;
using System.Security.Principal;
using System.Windows.Forms;
using Microsoft.Win32;

namespace OpenDesk.Installer
{
    public class SetupWizardForm : Form
    {
        private Panel panelHeader;
        private Label lblHeaderTitle;
        private Label lblHeaderSubtitle;
        private Panel panelBody;
        private Button btnBack;
        private Button btnNext;
        private Button btnCancel;
        private int currentStep = 1;

        // Step 1: Welcome
        private Label lblWelcomeTitle;
        private Label lblWelcomeDesc;

        // Step 2: Path
        private Label lblPathPrompt;
        private TextBox txtInstallPath;
        private Button btnBrowse;

        // Step 3: Options
        private CheckBox chkDesktopShortcut;
        private CheckBox chkStartMenuShortcut;
        private CheckBox chkAutoStart;

        // Step 4: Progress
        private ProgressBar progressBar;
        private Label lblStatus;

        // Step 5: Finish
        private Label lblFinishTitle;
        private CheckBox chkLaunchNow;

        private string defaultInstallPath;

        public SetupWizardForm()
        {
            InitializeComponent();
            ShowStep(1);
        }

        private void InitializeComponent()
        {
            this.Text = "Instalasi OpenDesk 2.0";
            this.Size = new Size(540, 420);
            this.StartPosition = FormStartPosition.CenterScreen;
            this.FormBorderStyle = FormBorderStyle.FixedDialog;
            this.MaximizeBox = false;
            this.BackColor = Color.FromArgb(24, 25, 32);
            this.ForeColor = Color.White;

            // Header Banner
            panelHeader = new Panel
            {
                Dock = DockStyle.Top,
                Height = 65,
                BackColor = Color.FromArgb(18, 19, 24)
            };
            lblHeaderTitle = new Label
            {
                Text = "Wizard Instalasi OpenDesk",
                Font = new Font("Segoe UI", 12, FontStyle.Bold),
                ForeColor = Color.FromArgb(239, 68, 68),
                Location = new Point(20, 12),
                AutoSize = true
            };
            lblHeaderSubtitle = new Label
            {
                Text = "Aplikasi Kontrol Jarak Jauh Native & Berkecepatan Tinggi",
                Font = new Font("Segoe UI", 9),
                ForeColor = Color.FromArgb(148, 163, 184),
                Location = new Point(20, 36),
                AutoSize = true
            };
            panelHeader.Controls.Add(lblHeaderTitle);
            panelHeader.Controls.Add(lblHeaderSubtitle);
            this.Controls.Add(panelHeader);

            // Bottom Buttons
            btnCancel = new Button
            {
                Text = "Batal",
                Location = new Point(420, 335),
                Size = new Size(85, 30),
                BackColor = Color.FromArgb(39, 40, 50),
                ForeColor = Color.White,
                FlatStyle = FlatStyle.Flat
            };
            btnCancel.Click += (s, e) => this.Close();

            btnNext = new Button
            {
                Text = "Lanjut >",
                Location = new Point(325, 335),
                Size = new Size(85, 30),
                BackColor = Color.FromArgb(225, 29, 72),
                ForeColor = Color.White,
                FlatStyle = FlatStyle.Flat
            };
            btnNext.Click += BtnNext_Click;

            btnBack = new Button
            {
                Text = "< Kembali",
                Location = new Point(230, 335),
                Size = new Size(85, 30),
                BackColor = Color.FromArgb(39, 40, 50),
                ForeColor = Color.White,
                FlatStyle = FlatStyle.Flat,
                Enabled = false
            };
            btnBack.Click += (s, e) => ShowStep(currentStep - 1);

            this.Controls.Add(btnCancel);
            this.Controls.Add(btnNext);
            this.Controls.Add(btnBack);

            // Body Area
            panelBody = new Panel
            {
                Location = new Point(20, 80),
                Size = new Size(485, 240),
                BackColor = Color.Transparent
            };
            this.Controls.Add(panelBody);

            // Compute default path
            bool isAdmin = new WindowsPrincipal(WindowsIdentity.GetCurrent()).IsInRole(WindowsBuiltInRole.Administrator);
            if (isAdmin)
            {
                string progFiles = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles);
                defaultInstallPath = Path.Combine(progFiles, "OpenDesk");
            }
            else
            {
                string localApp = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
                defaultInstallPath = Path.Combine(localApp, "Programs", "OpenDesk");
            }
        }

        private void ShowStep(int step)
        {
            currentStep = step;
            panelBody.Controls.Clear();
            btnBack.Enabled = (step > 1 && step < 4);

            if (step == 1)
            {
                lblWelcomeTitle = new Label
                {
                    Text = "Selamat Datang di Pengaturan OpenDesk",
                    Font = new Font("Segoe UI", 12, FontStyle.Bold),
                    ForeColor = Color.White,
                    Location = new Point(10, 20),
                    AutoSize = true
                };
                lblWelcomeDesc = new Label
                {
                    Text = "Wizard ini akan memandu Anda menginstal OpenDesk pada sistem Windows Anda.\n\n" +
                           "- Terintegrasi langsung ke Program Files & Menu Mulai\n" +
                           "- Penyimpanan riwayat & preferensi di %APPDATA%\\OpenDesk\n" +
                           "- Native Rust Engine tanpa dependensi eksternal\n\n" +
                           "Klik 'Lanjut' untuk melanjutkan.",
                    Font = new Font("Segoe UI", 9.5f),
                    ForeColor = Color.FromArgb(203, 213, 225),
                    Location = new Point(10, 60),
                    Size = new Size(460, 160)
                };
                panelBody.Controls.Add(lblWelcomeTitle);
                panelBody.Controls.Add(lblWelcomeDesc);
                btnNext.Text = "Lanjut >";
            }
            else if (step == 2)
            {
                lblPathPrompt = new Label
                {
                    Text = "Pilih Lokasi Folder Instalasi:",
                    Font = new Font("Segoe UI", 10, FontStyle.Bold),
                    ForeColor = Color.White,
                    Location = new Point(10, 20),
                    AutoSize = true
                };
                txtInstallPath = new TextBox
                {
                    Text = defaultInstallPath,
                    Location = new Point(10, 60),
                    Size = new Size(370, 26),
                    BackColor = Color.FromArgb(15, 16, 20),
                    ForeColor = Color.White,
                    Font = new Font("Segoe UI", 9.5f)
                };
                btnBrowse = new Button
                {
                    Text = "Cari...",
                    Location = new Point(390, 58),
                    Size = new Size(75, 28),
                    BackColor = Color.FromArgb(39, 40, 50),
                    ForeColor = Color.White,
                    FlatStyle = FlatStyle.Flat
                };
                btnBrowse.Click += (s, e) =>
                {
                    using (FolderBrowserDialog fbd = new FolderBrowserDialog())
                    {
                        fbd.SelectedPath = txtInstallPath.Text;
                        if (fbd.ShowDialog() == DialogResult.OK)
                        {
                            txtInstallPath.Text = fbd.SelectedPath;
                        }
                    }
                };
                panelBody.Controls.Add(lblPathPrompt);
                panelBody.Controls.Add(txtInstallPath);
                panelBody.Controls.Add(btnBrowse);
                btnNext.Text = "Lanjut >";
            }
            else if (step == 3)
            {
                Label lblOptTitle = new Label
                {
                    Text = "Pilih Opsi Tambahan:",
                    Font = new Font("Segoe UI", 10, FontStyle.Bold),
                    ForeColor = Color.White,
                    Location = new Point(10, 20),
                    AutoSize = true
                };
                chkDesktopShortcut = new CheckBox
                {
                    Text = "Buat pintasan di Desktop",
                    Checked = true,
                    Location = new Point(20, 60),
                    AutoSize = true,
                    ForeColor = Color.FromArgb(203, 213, 225)
                };
                chkStartMenuShortcut = new CheckBox
                {
                    Text = "Buat pintasan di Menu Mulai (Start Menu)",
                    Checked = true,
                    Location = new Point(20, 95),
                    AutoSize = true,
                    ForeColor = Color.FromArgb(203, 213, 225)
                };
                chkAutoStart = new CheckBox
                {
                    Text = "Jalankan OpenDesk saat Windows dinyalakan",
                    Checked = false,
                    Location = new Point(20, 130),
                    AutoSize = true,
                    ForeColor = Color.FromArgb(203, 213, 225)
                };
                panelBody.Controls.Add(lblOptTitle);
                panelBody.Controls.Add(chkDesktopShortcut);
                panelBody.Controls.Add(chkStartMenuShortcut);
                panelBody.Controls.Add(chkAutoStart);
                btnNext.Text = "Instal Sekarang";
            }
            else if (step == 4)
            {
                btnBack.Enabled = false;
                btnNext.Enabled = false;
                btnCancel.Enabled = false;

                lblStatus = new Label
                {
                    Text = "Menginstal OpenDesk...",
                    Font = new Font("Segoe UI", 10),
                    ForeColor = Color.White,
                    Location = new Point(10, 40),
                    AutoSize = true
                };
                progressBar = new ProgressBar
                {
                    Location = new Point(10, 80),
                    Size = new Size(460, 24),
                    Style = ProgressBarStyle.Marquee
                };
                panelBody.Controls.Add(lblStatus);
                panelBody.Controls.Add(progressBar);

                PerformInstall();
            }
            else if (step == 5)
            {
                btnBack.Visible = false;
                btnCancel.Visible = false;
                btnNext.Enabled = true;
                btnNext.Text = "Selesai";

                lblFinishTitle = new Label
                {
                    Text = "Instalasi OpenDesk Selesai!",
                    Font = new Font("Segoe UI", 12, FontStyle.Bold),
                    ForeColor = Color.FromArgb(74, 222, 128),
                    Location = new Point(10, 30),
                    AutoSize = true
                };
                Label lblFinishDesc = new Label
                {
                    Text = "OpenDesk telah berhasil diinstal pada perangkat Anda.\n" +
                           "Anda dapat mengaksesnya melalui Desktop atau Menu Mulai.",
                    Font = new Font("Segoe UI", 9.5f),
                    ForeColor = Color.FromArgb(203, 213, 225),
                    Location = new Point(10, 70),
                    Size = new Size(460, 60)
                };
                chkLaunchNow = new CheckBox
                {
                    Text = "Jalankan OpenDesk sekarang",
                    Checked = true,
                    Location = new Point(15, 140),
                    AutoSize = true,
                    ForeColor = Color.White
                };
                panelBody.Controls.Add(lblFinishTitle);
                panelBody.Controls.Add(lblFinishDesc);
                panelBody.Controls.Add(chkLaunchNow);
            }
        }

        private void BtnNext_Click(object sender, EventArgs e)
        {
            if (currentStep == 1) ShowStep(2);
            else if (currentStep == 2) ShowStep(3);
            else if (currentStep == 3) ShowStep(4);
            else if (currentStep == 5)
            {
                if (chkLaunchNow != null && chkLaunchNow.Checked)
                {
                    string targetExe = Path.Combine(txtInstallPath.Text, "OpenDesk.exe");
                    if (File.Exists(targetExe))
                    {
                        System.Diagnostics.Process.Start(targetExe);
                    }
                }
                this.Close();
            }
        }

        private void PerformInstall()
        {
            Timer t = new Timer { Interval = 600 };
            t.Tick += (s, e) =>
            {
                t.Stop();
                try
                {
                    string destDir = txtInstallPath.Text;
                    Directory.CreateDirectory(destDir);

                    // Extract embedded payload OpenDesk.exe
                    string destExe = Path.Combine(destDir, "OpenDesk.exe");
                    Assembly asm = Assembly.GetExecutingAssembly();
                    using (Stream src = asm.GetManifestResourceStream("payload.exe"))
                    {
                        if (src != null)
                        {
                            using (FileStream fs = new FileStream(destExe, FileMode.Create, FileAccess.Write))
                            {
                                src.CopyTo(fs);
                            }
                        }
                    }

                    // Create AppData folder
                    string appData = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "OpenDesk");
                    Directory.CreateDirectory(appData);

                    // Create Shortcuts via VBScript WScript.Shell
                    CreateShortcuts(destExe, chkDesktopShortcut.Checked, chkStartMenuShortcut.Checked);

                    // Register Uninstall
                    RegisterUninstaller(destDir, destExe);

                    // Autostart registry
                    if (chkAutoStart.Checked)
                    {
                        using (RegistryKey rk = Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Run", true))
                        {
                            if (rk != null) rk.SetValue("OpenDesk", "\"" + destExe + "\"");
                        }
                    }

                    ShowStep(5);
                }
                catch (Exception ex)
                {
                    MessageBox.Show("Error saat instalasi: " + ex.Message, "Instalasi Gagal", MessageBoxButtons.OK, MessageBoxIcon.Error);
                    this.Close();
                }
            };
            t.Start();
        }

        private void CreateShortcuts(string targetExe, bool desktop, bool startMenu)
        {
            try
            {
                Type shellType = Type.GetTypeFromProgID("WScript.Shell");
                if (shellType == null) return;
                dynamic shell = Activator.CreateInstance(shellType);

                if (desktop)
                {
                    string deskPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Desktop), "OpenDesk.lnk");
                    dynamic link = shell.CreateShortcut(deskPath);
                    link.TargetPath = targetExe;
                    link.WorkingDirectory = Path.GetDirectoryName(targetExe);
                    link.Description = "OpenDesk Remote Desktop";
                    link.Save();
                }

                if (startMenu)
                {
                    string startPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.StartMenu), "Programs", "OpenDesk.lnk");
                    dynamic link = shell.CreateShortcut(startPath);
                    link.TargetPath = targetExe;
                    link.WorkingDirectory = Path.GetDirectoryName(targetExe);
                    link.Description = "OpenDesk Remote Desktop";
                    link.Save();
                }
            }
            catch { }
        }

        private void RegisterUninstaller(string installDir, string exePath)
        {
            try
            {
                using (RegistryKey key = Registry.CurrentUser.CreateSubKey(@"Software\Microsoft\Windows\CurrentVersion\Uninstall\OpenDesk"))
                {
                    if (key != null)
                    {
                        key.SetValue("DisplayName", "OpenDesk 2.0");
                        key.SetValue("DisplayVersion", "2.0.0");
                        key.SetValue("Publisher", "OpenDesk Team");
                        key.SetValue("DisplayIcon", exePath + ",0");
                        key.SetValue("InstallLocation", installDir);
                        key.SetValue("UninstallString", "\"" + exePath + "\" --uninstall");
                        key.SetValue("NoModify", 1, RegistryValueKind.DWord);
                        key.SetValue("NoRepair", 1, RegistryValueKind.DWord);
                    }
                }
            }
            catch { }
        }

        [STAThread]
        static void Main()
        {
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            Application.Run(new SetupWizardForm());
        }
    }
}
