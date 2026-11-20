// Claudy 시험 격리 도우미 (Windows). Claudy 서버가 처음 쓸 때 사용자 폴더 .claudy\tools 에 DLL 로 컴파일한다.
// 목적: 시험 프로그램을 사용자가 보지 않는 가상 데스크톱(기본 「Claudy 시험」)에서 돌리고, 사용자의 마우스·키보드·화면을 건드리지 않는다.
// - 데스크톱 만들기·창 옮기기는 explorer 의 IVirtualDesktopManagerInternal(Windows 11 24H2 이후 IID 53F5CA0B)을 쓴다.
//   IID 가 다른 빌드에서는 QueryService 가 실패하므로 아무것도 하지 않고 오류를 돌려준다.
// - 실제 입력(SendInput·SetCursorPos)과 SetForegroundWindow 는 쓰지 않는다. 데스크톱 전환은 감시기가 사용자 화면을 되돌릴 때만 쓴다.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace ClaudyDesk
{
    [ComImport, Guid("6D5140C1-7436-11CE-8034-00AA006009FA"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IServiceProvider10 { [PreserveSig] int QueryService(ref Guid service, ref Guid riid, [MarshalAs(UnmanagedType.IUnknown)] out object obj); }

    [ComImport, Guid("92CA9DCD-5622-4BBA-A805-5E9F541BD8C9"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IObjectArray { [PreserveSig] int GetCount(out int count); [PreserveSig] int GetAt(int index, ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object obj); }

    [ComImport, Guid("372E1D3B-38D3-42E4-A15B-8AB2B178F513"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationView { }

    [ComImport, Guid("1841C6D7-4F9D-42C0-AF41-8747538F10E5"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationViewCollection
    {
        [PreserveSig] int GetViews(out IObjectArray a);
        [PreserveSig] int GetViewsByZOrder(out IObjectArray a);
        [PreserveSig] int GetViewsByAppUserModelId([MarshalAs(UnmanagedType.LPWStr)] string id, out IObjectArray a);
        [PreserveSig] int GetViewForHwnd(IntPtr hwnd, out IApplicationView view);
    }

    [ComImport, Guid("3F07F4BE-B107-441A-AF0F-39D82529072C"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IVirtualDesktop
    {
        [PreserveSig] int IsViewVisible(IApplicationView view, out int visible);
        [PreserveSig] int GetId(out Guid id);
        [PreserveSig] int GetName([MarshalAs(UnmanagedType.HString)] out string name);
    }

    [ComImport, Guid("53F5CA0B-158F-4124-900C-057158060B27"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IVirtualDesktopManagerInternal
    {
        [PreserveSig] int GetCount(out int count);
        [PreserveSig] int MoveViewToDesktop(IApplicationView view, IVirtualDesktop desktop);
        [PreserveSig] int CanViewMoveDesktops(IApplicationView view, out int can);
        [PreserveSig] int GetCurrentDesktop(out IVirtualDesktop d);
        [PreserveSig] int GetDesktops(out IObjectArray a);
        [PreserveSig] int GetAdjacentDesktop(IVirtualDesktop from, int direction, out IVirtualDesktop d);
        [PreserveSig] int SwitchDesktop(IVirtualDesktop d);
        [PreserveSig] int SwitchDesktopAndMoveForegroundView_DoNotCall(IVirtualDesktop d);
        [PreserveSig] int CreateDesktop(out IVirtualDesktop d);
        [PreserveSig] int MoveDesktop(IVirtualDesktop d, int index);
        [PreserveSig] int RemoveDesktop_DoNotCall(IVirtualDesktop d, IVirtualDesktop fallback);
        [PreserveSig] int FindDesktop(ref Guid id, out IVirtualDesktop d);
        [PreserveSig] int GetDesktopSwitchIncludeExcludeViews(IVirtualDesktop d, out IObjectArray a, out IObjectArray b);
        [PreserveSig] int SetDesktopName(IVirtualDesktop d, [MarshalAs(UnmanagedType.HString)] string name);
    }

    [ComImport, Guid("A5CD92FF-29BE-454C-8D04-D82879FB3F1B"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IVirtualDesktopManager
    {
        [PreserveSig] int IsWindowOnCurrentVirtualDesktop(IntPtr h, out int on);
        [PreserveSig] int GetWindowDesktopId(IntPtr h, out Guid id);
        [PreserveSig] int MoveWindowToDesktop(IntPtr h, ref Guid id);
    }

    static class W
    {
        public delegate bool EnumProc(IntPtr h, IntPtr l);
        [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
        [DllImport("user32.dll")] public static extern int GetWindowThreadProcessId(IntPtr h, out int pid);
        [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
        [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
        [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
        [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint f);
        [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint cmd);
        [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
        [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr h, int cmd);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
        [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
        [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
        [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int v, int size);
        [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        public struct STARTUPINFO { public int cb; public string lpReserved, lpDesktop, lpTitle; public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags; public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError; }
        [StructLayout(LayoutKind.Sequential)] public struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }
        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        public static extern bool CreateProcess(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
        [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        public struct PROCESSENTRY32 { public int dwSize, cntUsage, th32ProcessID; public IntPtr th32DefaultHeapID; public int th32ModuleID, cntThreads, th32ParentProcessID, pcPriClassBase, dwFlags; [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szExeFile; }
        [DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr CreateToolhelp32Snapshot(uint flags, int pid);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern bool Process32First(IntPtr h, ref PROCESSENTRY32 e);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern bool Process32Next(IntPtr h, ref PROCESSENTRY32 e);
        public static string Cls(IntPtr h) { var sb = new StringBuilder(256); GetClassName(h, sb, 256); return sb.ToString(); }
        public static string Title(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
        public static int Pid(IntPtr h) { int p; GetWindowThreadProcessId(h, out p); return p; }
        public static bool Cloaked(IntPtr h) { int c; return DwmGetWindowAttribute(h, 14, out c, 4) == 0 && c != 0; }
        /// pid → (부모 pid, 실행 파일 이름)
        public static Dictionary<int, KeyValuePair<int, string>> Procs()
        {
            var d = new Dictionary<int, KeyValuePair<int, string>>();
            IntPtr s = CreateToolhelp32Snapshot(2, 0);
            if (s == IntPtr.Zero || s == new IntPtr(-1)) return d;
            var e = new PROCESSENTRY32(); e.dwSize = Marshal.SizeOf(typeof(PROCESSENTRY32));
            if (Process32First(s, ref e)) do { d[e.th32ProcessID] = new KeyValuePair<int, string>(e.th32ParentProcessID, e.szExeFile); } while (Process32Next(s, ref e));
            CloseHandle(s);
            return d;
        }
    }

    public static class D
    {
        public const string DefaultName = "Claudy 시험";
        static readonly Guid CLSID_ImmersiveShell = new Guid("C2F03A33-21F5-47FA-B4BB-156362A2F239");
        static readonly Guid SID_VDMI = new Guid("C5E0CDCA-7B6E-41B2-9FC4-D93975CC467B");
        static readonly Guid CLSID_VDM = new Guid("AA509086-5CA9-4C25-8F95-589D3C07B48A");
        [ThreadStatic] static IVirtualDesktopManagerInternal mi;
        [ThreadStatic] static IApplicationViewCollection views;
        [ThreadStatic] static IVirtualDesktopManager pub;
        public static string LastError = "";

        static string Base { get { return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".claudy"); } }
        public static string PidFile { get { return Path.Combine(Base, "isolate", "pids.txt"); } }
        public static string LogFile { get { return Path.Combine(Base, "isolate.log"); } }

        public static void Log(string msg)
        {
            try { Directory.CreateDirectory(Base); File.AppendAllText(LogFile, DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + " " + msg + "\r\n", new UTF8Encoding(false)); } catch { }
        }

        static void Init(bool fresh)
        {
            if (!fresh && mi != null && views != null && pub != null) return;
            mi = null; views = null; pub = null;
            var sp = (IServiceProvider10)Activator.CreateInstance(Type.GetTypeFromCLSID(CLSID_ImmersiveShell));
            Guid s = SID_VDMI, i = typeof(IVirtualDesktopManagerInternal).GUID; object o;
            int hr = sp.QueryService(ref s, ref i, out o);
            if (hr != 0 || o == null) throw new Exception(string.Format("이 Windows 빌드에서는 가상 데스크톱 내부 기능(53F5CA0B)을 쓸 수 없다 hr=0x{0:X8}. Windows 11 24H2 이후가 필요하다", hr));
            mi = (IVirtualDesktopManagerInternal)o;
            s = typeof(IApplicationViewCollection).GUID; i = s;
            hr = sp.QueryService(ref s, ref i, out o);
            if (hr != 0 || o == null) throw new Exception(string.Format("IApplicationViewCollection 없음 hr=0x{0:X8}", hr));
            views = (IApplicationViewCollection)o;
            pub = (IVirtualDesktopManager)Activator.CreateInstance(Type.GetTypeFromCLSID(CLSID_VDM));
        }

        static T Retry<T>(Func<T> f)
        {
            try { Init(false); return f(); }
            catch (COMException) { Init(true); return f(); }
            catch (InvalidComObjectException) { Init(true); return f(); }
        }

        static List<IVirtualDesktop> Desktops()
        {
            IObjectArray a; int hr = mi.GetDesktops(out a); if (hr != 0) throw new COMException("GetDesktops", hr);
            int n; a.GetCount(out n);
            var list = new List<IVirtualDesktop>();
            Guid iid = typeof(IVirtualDesktop).GUID;
            for (int k = 0; k < n; k++) { object o; a.GetAt(k, ref iid, out o); list.Add((IVirtualDesktop)o); }
            return list;
        }

        static Guid IdOf(IVirtualDesktop d) { Guid g; d.GetId(out g); return g; }
        static string NameOf(IVirtualDesktop d) { string s = ""; try { d.GetName(out s); } catch { } return s ?? ""; }

        /// 사용자가 보는 데스크톱 번호(1부터)
        public static int Current()
        {
            return Retry(() => { IVirtualDesktop c; mi.GetCurrentDesktop(out c); Guid g = IdOf(c); var l = Desktops(); for (int k = 0; k < l.Count; k++) if (IdOf(l[k]) == g) return k + 1; return 0; });
        }

        /// 이름이 name 인 데스크톱 번호. 없으면 맨 뒤에 만들어 이름을 붙인다(전환하지 않음)
        public static int Ensure(string name)
        {
            if (string.IsNullOrEmpty(name)) name = DefaultName;
            return Retry(() =>
            {
                var l = Desktops();
                for (int k = 0; k < l.Count; k++) if (NameOf(l[k]) == name) return k + 1;
                IVirtualDesktop d; int hr = mi.CreateDesktop(out d); if (hr != 0) throw new COMException("CreateDesktop", hr);
                mi.SetDesktopName(d, name);
                Log("가상 데스크톱 「" + name + "」 을 만듦(전환 없음)");
                return Desktops().Count;
            });
        }

        /// 데스크톱 목록. 번호, 이름, 사용자가 보는 중인지
        public static string ListDesktops()
        {
            return Retry(() =>
            {
                var l = Desktops(); int cur = Current(); var sb = new StringBuilder();
                for (int k = 0; k < l.Count; k++) sb.AppendFormat("데스크톱 {0} 「{1}」{2}\n", k + 1, NameOf(l[k]), k + 1 == cur ? " ← 사용자가 보는 중" : "");
                return sb.ToString();
            });
        }

        /// 창이 있는 데스크톱 번호. 모든 데스크톱 고정·모름은 0, 실패는 -1
        public static int NumberOf(IntPtr h)
        {
            return Retry(() =>
            {
                Guid g; int hr = pub.GetWindowDesktopId(h, out g);
                if (hr != 0) return -1;
                var l = Desktops(); for (int k = 0; k < l.Count; k++) if (IdOf(l[k]) == g) return k + 1;
                return 0;
            });
        }

        public static bool Move(IntPtr h, int n)
        {
            return Retry(() =>
            {
                LastError = "";
                if (!W.IsWindow(h)) { LastError = "창 없음"; return false; }
                IntPtr root = W.GetAncestor(h, 2); if (root != IntPtr.Zero) h = root;
                var l = Desktops(); if (n < 1 || n > l.Count) { LastError = "데스크톱 " + n + " 없음"; return false; }
                Guid want = IdOf(l[n - 1]); Guid cur;
                if (pub.GetWindowDesktopId(h, out cur) == 0 && cur == want) return true;
                IApplicationView v; int hr = views.GetViewForHwnd(h, out v);
                if (hr != 0 || v == null) { LastError = string.Format("보기 없음 0x{0:X8}", hr); return false; }
                hr = mi.MoveViewToDesktop(v, l[n - 1]);
                if (hr != 0) { LastError = string.Format("MoveViewToDesktop 0x{0:X8}", hr); return false; }
                return true;
            });
        }

        static bool SwitchTo(int n)
        {
            return Retry(() => { var l = Desktops(); if (n < 1 || n > l.Count) return false; return mi.SwitchDesktop(l[n - 1]) == 0; });
        }

        /// 숨은 데스크톱으로 옮긴 창을 비활성으로 펼친다. 최소화 창은 그림을 그리지 않아(브라우저 화면 찍기·렌더링이 멈춤) 시험이 안 된다.
        /// 사용자가 보는 데스크톱에 있는 창은 펼치지 않는다
        static void Unfold(IntPtr h, int n)
        {
            try { if (W.IsIconic(h) && NumberOf(h) == n && Current() != n) W.ShowWindowAsync(h, 4); } catch { }
        }

        public static IntPtr[] TopWindows(int pid)
        {
            var list = new List<IntPtr>();
            W.EnumWindows(delegate (IntPtr h, IntPtr l) { if (W.Pid(h) == pid && W.IsWindowVisible(h)) list.Add(h); return true; }, IntPtr.Zero);
            return list.ToArray();
        }

        // ---- 격리 대상 PID 목록(파일). 감시기와 도구가 같이 쓴다
        public static HashSet<int> Watched()
        {
            var s = new HashSet<int>();
            try { foreach (var line in File.ReadAllLines(PidFile)) { var p = line.Split('\t'); int v; if (p.Length > 0 && int.TryParse(p[0], out v)) s.Add(v); } } catch { }
            return s;
        }
        static string DesktopFor(int pid)
        {
            try { foreach (var line in File.ReadAllLines(PidFile)) { var p = line.Split('\t'); if (p.Length > 1 && p[0] == pid.ToString()) return p[1]; } } catch { }
            return DefaultName;
        }
        public static void AddWatch(int pid, string name)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(PidFile));
            var keep = new List<string>();
            var alive = W.Procs();
            try { foreach (var line in File.ReadAllLines(PidFile)) { int v; var p = line.Split('\t'); if (int.TryParse(p[0], out v) && alive.ContainsKey(v) && v != pid) keep.Add(line); } } catch { }
            keep.Add(pid + "\t" + name);
            File.WriteAllLines(PidFile, keep.ToArray(), new UTF8Encoding(false));
        }

        /// pid 가 격리 대상이거나 그 자손이면 대상 데스크톱 이름, 아니면 null
        static string IsolatedRoot(int pid, HashSet<int> watched, Dictionary<int, KeyValuePair<int, string>> procs)
        {
            int p = pid;
            for (int depth = 0; depth < 6 && p > 4; depth++)
            {
                if (watched.Contains(p))
                {
                    string name = DesktopFor(p);
                    if (p != pid) { try { AddWatch(pid, name); watched.Add(pid); } catch { } }
                    return name;
                }
                KeyValuePair<int, string> e; if (!procs.TryGetValue(p, out e)) break;
                p = e.Key;
            }
            return null;
        }

        /// 최소화·비활성으로 띄우고 주 창을 대상 데스크톱으로 보냄. 감시 목록에 올림. 결과 "pid 옮긴창수"
        public static string Launch(string exe, string args, string cwd, string name, int waitMs)
        {
            int n = Ensure(name);
            var si = new W.STARTUPINFO(); si.cb = Marshal.SizeOf(typeof(W.STARTUPINFO));
            si.dwFlags = 1; si.wShowWindow = 7; // SW_SHOWMINNOACTIVE
            var cmd = new StringBuilder("\"" + exe + "\" " + (args ?? ""), 8192);
            W.PROCESS_INFORMATION pi;
            if (!W.CreateProcess(null, cmd, IntPtr.Zero, IntPtr.Zero, false, 0x08000000, IntPtr.Zero, string.IsNullOrEmpty(cwd) ? null : cwd, ref si, out pi))
                throw new Exception("CreateProcess 실패 " + Marshal.GetLastWin32Error());
            W.CloseHandle(pi.hThread); W.CloseHandle(pi.hProcess);
            AddWatch(pi.dwProcessId, name);
            Log("격리 실행 pid " + pi.dwProcessId + " 「" + name + "」 " + exe);
            int moved = 0; var done = new HashSet<long>(); bool exited = false;
            var end = DateTime.UtcNow.AddMilliseconds(waitMs);
            while (DateTime.UtcNow < end)
            {
                var procs = W.Procs(); var watched = Watched();
                if (!procs.ContainsKey(pi.dwProcessId)) exited = true;
                W.EnumWindows(delegate (IntPtr h, IntPtr l)
                {
                    if (!W.IsWindowVisible(h) || done.Contains((long)h) || W.GetWindow(h, 4) != IntPtr.Zero) return true;
                    int wp = W.Pid(h);
                    if (wp != pi.dwProcessId && IsolatedRoot(wp, watched, procs) == null) return true;
                    if (Move(h, n)) { done.Add((long)h); moved++; Unfold(h, n); }
                    return true;
                }, IntPtr.Zero);
                if (moved > 0) break;
                Thread.Sleep(20);
            }
            return pi.dwProcessId + " " + moved + " " + (exited ? "exited" : "running");
        }

        /// 이미 떠 있는 프로세스를 격리 대상으로 올리고 창을 옮김
        public static string Adopt(int pid, string name)
        {
            int n = Ensure(name); AddWatch(pid, name); int ok = 0;
            foreach (var h in TopWindows(pid)) { if (W.GetWindow(h, 4) != IntPtr.Zero) continue; if (Move(h, n)) { ok++; Unfold(h, n); } }
            Log("격리 대상으로 올림 pid " + pid + " 창 " + ok);
            return pid + " " + ok;
        }

        static string Esc(string s) { return (s ?? "").Replace("\\", "\\\\").Replace("\"", "\\\"").Replace("\n", " ").Replace("\r", " ").Replace("\t", " "); }

        /// 보이는 최상위 창 목록(JSON 배열). all 이 false 면 격리 대상 프로세스와 그 자손만
        public static string ListWindows(int pidFilter, bool all)
        {
            Dpi();
            var procs = W.Procs(); var watched = Watched(); var fg = W.GetForegroundWindow();
            var sb = new StringBuilder("["); bool first = true;
            W.EnumWindows(delegate (IntPtr h, IntPtr l)
            {
                if (!W.IsWindowVisible(h)) return true;
                int pid = W.Pid(h);
                if (pidFilter > 0 && pid != pidFilter) return true;
                string iso = IsolatedRoot(pid, watched, procs);
                if (!all && pidFilter <= 0 && iso == null) return true;
                string title = W.Title(h); if (title.Length == 0 && W.GetWindow(h, 4) == IntPtr.Zero && all) return true;
                W.RECT r; W.GetWindowRect(h, out r);
                KeyValuePair<int, string> pe; string exe = procs.TryGetValue(pid, out pe) ? pe.Value : "";
                if (!first) sb.Append(","); first = false;
                sb.AppendFormat("{{\"hwnd\":\"0x{0:X}\",\"pid\":{1},\"exe\":\"{2}\",\"title\":\"{3}\",\"class\":\"{4}\",\"desktop\":{5},\"isolated\":{6},\"minimized\":{7},\"owned\":{8},\"foreground\":{9},\"rect\":[{10},{11},{12},{13}]}}",
                    (long)h, pid, Esc(exe), Esc(title), Esc(W.Cls(h)), NumberOf(h), iso != null ? "true" : "false", W.IsIconic(h) ? "true" : "false",
                    W.GetWindow(h, 4) != IntPtr.Zero ? "true" : "false", h == fg ? "true" : "false", r.L, r.T, r.R - r.L, r.B - r.T);
                return true;
            }, IntPtr.Zero);
            return sb.Append("]").ToString();
        }

        /// 창 하나를 PrintWindow(PW_RENDERFULLCONTENT)로 찍어 PNG 로 저장. 결과 "가로 세로 검은비율%"
        public static string Capture(IntPtr h, string path, int maxWidth)
        {
            Dpi();
            if (!W.IsWindow(h)) throw new Exception("창 없음");
            bool wasMin = W.IsIconic(h);
            if (wasMin) { W.ShowWindowAsync(h, 4); Thread.Sleep(300); } // SW_SHOWNOACTIVATE. 최소화 창은 그림이 없으므로 비활성으로 펼침
            try
            {
                W.RECT r; W.GetWindowRect(h, out r);
                int w = Math.Max(1, r.R - r.L), ht = Math.Max(1, r.B - r.T);
                using (var bmp = new Bitmap(w, ht, PixelFormat.Format32bppArgb))
                {
                    using (var g = Graphics.FromImage(bmp))
                    {
                        IntPtr dc = g.GetHdc();
                        bool ok = W.PrintWindow(h, dc, 2);
                        g.ReleaseHdc(dc);
                        if (!ok) throw new Exception("PrintWindow 실패");
                    }
                    int dark = 0, total = 0;
                    for (int y = 0; y < ht; y += Math.Max(1, ht / 40)) for (int x = 0; x < w; x += Math.Max(1, w / 40)) { var c = bmp.GetPixel(x, y); total++; if (c.R + c.G + c.B < 24) dark++; }
                    Bitmap outBmp = bmp;
                    if (maxWidth > 0 && w > maxWidth) { outBmp = new Bitmap(bmp, new Size(maxWidth, (int)((long)ht * maxWidth / w))); }
                    outBmp.Save(path, ImageFormat.Png);
                    int dw = outBmp.Width, dh = outBmp.Height;
                    if (outBmp != bmp) outBmp.Dispose();
                    return dw + " " + dh + " " + (total > 0 ? dark * 100 / total : 0);
                }
            }
            finally { if (wasMin && NumberOf(h) == Current()) W.ShowWindowAsync(h, 7); } // 사용자가 보는 데스크톱의 창만 다시 최소화
        }


        // ---- 입력. 실제 마우스·키보드(SendInput, SetCursorPos)를 쓰지 않고 대상 창과 그 자식 창(단추·입력란)에 Windows 메시지를 보낸다.
        // 사용자가 보는 데스크톱의 창에는 보내지 않는다. 좌표는 창 왼쪽 위 기준 픽셀(window_capture 그림과 같은 기준)
        [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
        [DllImport("user32.dll")] static extern IntPtr ChildWindowFromPointEx(IntPtr h, POINT pt, uint flags);
        [DllImport("user32.dll")] static extern bool ScreenToClient(IntPtr h, ref POINT pt);
        [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(int tid, ref GUITHREADINFO gi);
        [DllImport("user32.dll")] static extern int GetWindowThreadProcessId(IntPtr h, IntPtr pid);
        [DllImport("user32.dll")] static extern IntPtr SetWindowLongPtr(IntPtr h, int idx, IntPtr v);
        [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint type);
        [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; public POINT(int x, int y) { X = x; Y = y; } }
        [StructLayout(LayoutKind.Sequential)] struct GUITHREADINFO { public int cbSize, flags; public IntPtr hwndActive, hwndFocus, hwndCapture, hwndMenuOwner, hwndMoveSize, hwndCaret; public W.RECT rcCaret; }

        [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
        /// 좌표를 모두 실제 픽셀로 받는다. PowerShell 은 DPI 비인식이라 최상위 창 좌표만 배율이 적용된 값으로 와서 자식 창 좌표와 어긋났다(11.20 실측)
        public static void Dpi() { try { SetThreadDpiAwarenessContext(new IntPtr(-4)); } catch { } }

        static IntPtr Guarded(IntPtr h)
        {
            Dpi();
            if (!W.IsWindow(h)) throw new Exception("창 없음");
            IntPtr root = W.GetAncestor(h, 2); if (root == IntPtr.Zero) root = h;
            int n = NumberOf(root), cur = Current();
            if (n == cur || n == 0) throw new Exception("사용자가 보는 데스크톱(또는 모든 데스크톱에 고정)의 창이라 보내지 않는다. 격리 실행한 창에만 쓴다");
            if (W.IsIconic(root)) { W.ShowWindowAsync(root, 4); Thread.Sleep(300); } // 숨은 데스크톱에서 비활성으로 펼침
            return root;
        }

        static IntPtr MakeL(int x, int y) { return new IntPtr((y << 16) | (x & 0xFFFF)); }

        /// 창 좌표(x, y)에 있는 가장 깊은 자식 창과 그 창의 클라이언트 좌표
        static IntPtr ChildAt(IntPtr root, int x, int y, out int cx, out int cy)
        {
            W.RECT r; W.GetWindowRect(root, out r);
            var sp = new POINT(r.L + x, r.T + y);
            IntPtr cur = root;
            for (int i = 0; i < 16; i++)
            {
                var pt = sp; ScreenToClient(cur, ref pt);
                IntPtr c = ChildWindowFromPointEx(cur, pt, 1 | 4); // CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT
                if (c == IntPtr.Zero || c == cur) break;
                cur = c;
            }
            var cp = sp; ScreenToClient(cur, ref cp); cx = cp.X; cy = cp.Y;
            return cur;
        }

        /// 입력을 받을 창. 좌표가 있으면 그 위치의 자식 창, 없으면 그 창 스레드의 포커스 창
        static IntPtr InputTarget(IntPtr root, int x, int y)
        {
            int cx, cy;
            if (x >= 0 && y >= 0) return ChildAt(root, x, y, out cx, out cy);
            var gi = new GUITHREADINFO(); gi.cbSize = Marshal.SizeOf(typeof(GUITHREADINFO));
            if (GetGUIThreadInfo(GetWindowThreadProcessId(root, IntPtr.Zero), ref gi) && gi.hwndFocus != IntPtr.Zero) return gi.hwndFocus;
            return root;
        }

        /// 클릭. button 은 left·right, count 는 1·2
        public static string Click(IntPtr h, int x, int y, string button, int count, bool pointer)
        {
            IntPtr root = Guarded(h); int cx, cy;
            IntPtr t = ChildAt(root, x, y, out cx, out cy);
            IntPtr l = MakeL(cx, cy);
            bool right = button == "right";
            uint down = right ? 0x0204u : 0x0201u, up = right ? 0x0205u : 0x0202u, dbl = right ? 0x0206u : 0x0203u;
            IntPtr mk = new IntPtr(right ? 2 : 1);
            if (pointer) Pointer(root, x, y);
            PostMessage(t, 0x0200, IntPtr.Zero, l); // WM_MOUSEMOVE
            PostMessage(t, down, mk, l); PostMessage(t, up, IntPtr.Zero, l);
            if (count >= 2) { PostMessage(t, dbl, mk, l); PostMessage(t, up, IntPtr.Zero, l); }
            if (pointer) PointerEnd(root);
            return string.Format("{0} {1}회 → 0x{2:X} 「{3}」 ({4},{5})", right ? "오른쪽 클릭" : "클릭", count, (long)t, W.Cls(t), cx, cy);
        }

        public static string Scroll(IntPtr h, int x, int y, int notches, bool pointer)
        {
            IntPtr root = Guarded(h); int cx, cy;
            IntPtr t = ChildAt(root, x, y, out cx, out cy);
            W.RECT r; W.GetWindowRect(root, out r);
            if (pointer) Pointer(root, x, y);
            PostMessage(t, 0x020A, new IntPtr((notches * 120) << 16), MakeL(r.L + x, r.T + y)); // WM_MOUSEWHEEL, 화면 좌표
            if (pointer) PointerEnd(root);
            return string.Format("휠 {0}칸 → 0x{1:X} 「{2}」", notches, (long)t, W.Cls(t));
        }

        /// 글자 입력(WM_CHAR). 줄바꿈은 Enter
        public static string TypeText(IntPtr h, int x, int y, string text)
        {
            IntPtr root = Guarded(h); IntPtr t = InputTarget(root, x, y);
            foreach (char ch in text.Replace("\r\n", "\n"))
            {
                if (ch == '\n') { KeyTo(t, 0x0D, false); continue; }
                PostMessage(t, 0x0102, new IntPtr(ch), new IntPtr(1));
            }
            return string.Format("{0}자 입력 → 0x{1:X} 「{2}」", text.Length, (long)t, W.Cls(t));
        }

        static void KeyTo(IntPtr t, int vk, bool alt)
        {
            uint sc = MapVirtualKey((uint)vk, 0);
            long ld = 1 | ((long)sc << 16) | (alt ? (1L << 29) : 0);
            long lu = ld | (1L << 30) | (1L << 31);
            PostMessage(t, alt ? 0x0104u : 0x0100u, new IntPtr(vk), new IntPtr(ld));
            if (!alt && vk == 0x0D) PostMessage(t, 0x0102, new IntPtr(13), new IntPtr(ld));
            PostMessage(t, alt ? 0x0105u : 0x0101u, new IntPtr(vk), new IntPtr((int)lu));
        }

        static int Vk(string k)
        {
            switch (k)
            {
                case "enter": return 0x0D; case "tab": return 0x09; case "esc": case "escape": return 0x1B; case "space": return 0x20;
                case "backspace": return 0x08; case "delete": case "del": return 0x2E; case "insert": return 0x2D;
                case "up": return 0x26; case "down": return 0x28; case "left": return 0x25; case "right": return 0x27;
                case "home": return 0x24; case "end": return 0x23; case "pageup": return 0x21; case "pagedown": return 0x22;
            }
            if (k.Length > 1 && k[0] == 'f') { int f; if (int.TryParse(k.Substring(1), out f) && f >= 1 && f <= 12) return 0x70 + f - 1; }
            if (k.Length == 1) { char c = char.ToUpperInvariant(k[0]); if ((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')) return c; }
            throw new Exception("모르는 키: " + k);
        }

        /// 키 입력. 예 enter, tab, f5, alt+n. ctrl+·shift+ 조합은 메시지 방식에서 수식 키 상태가 전달되지 않아 받지 않는다
        public static string SendKey(IntPtr h, int x, int y, string key)
        {
            IntPtr root = Guarded(h); IntPtr t = InputTarget(root, x, y);
            string k = key.Trim().ToLowerInvariant().Replace(" ", "");
            bool alt = false;
            if (k.StartsWith("alt+")) { alt = true; k = k.Substring(4); }
            if (k.StartsWith("ctrl+") || k.StartsWith("shift+") || k.StartsWith("win+"))
                throw new Exception("ctrl·shift·win 조합은 메시지로 보낼 수 없다. 메뉴·단추는 invoke 로, 글자는 type·set_text 로 한다");
            KeyTo(alt ? root : t, Vk(k), alt);
            return string.Format("키 {0} → 0x{1:X} 「{2}」", key, (long)(alt ? root : t), W.Cls(alt ? root : t));
        }


        // 숨은 가상 데스크톱의 창은 Windows 가 「가린 창(cloaked)」으로 다루어 UI Automation 이 자식 요소를 내주지 않는다(11.20 문자표 실측).
        // 그래서 Win32 자식 창(단추·입력란 등)을 직접 찾아 메시지로 다룬다. 자식 창이 없는 앱(WPF·Chromium·egui)은 좌표 방식이나 앱 자체 통로를 쓴다
        [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr p, W.EnumProc f, IntPtr l);
        [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int idx);
        [DllImport("user32.dll")] static extern bool IsWindowEnabled(IntPtr h);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr SendMessageTimeout(IntPtr h, uint msg, IntPtr w, string l, uint flags, uint ms, out IntPtr res);
        [DllImport("user32.dll")] static extern IntPtr SendMessageTimeout(IntPtr h, uint msg, IntPtr w, IntPtr l, uint flags, uint ms, out IntPtr res);

        static string Clean(string t) { return t.Replace("&&", "\u0001").Replace("&", "").Replace("\u0001", "&"); }

        class Ctrl { public IntPtr H; public string Cls, Text, Kind; public int X, Y, Wd, Ht; public bool Enabled; }

        static string KindOf(IntPtr h, string cls)
        {
            string c = cls.ToLowerInvariant();
            if (c == "button")
            {
                int t = GetWindowLong(h, -16) & 0xF;
                if (t == 2 || t == 3 || t == 5 || t == 6) return "확인란";
                if (t == 4 || t == 9) return "선택 단추";
                if (t == 7) return "묶음";
                return "단추";
            }
            if (c == "edit" || c.StartsWith("richedit")) return "입력란";
            if (c == "combobox" || c == "comboboxex32") return "콤보";
            if (c == "listbox" || c == "syslistview32") return "목록";
            if (c == "static") return "글자";
            if (c == "systreeview32") return "트리";
            if (c == "systabcontrol32") return "탭";
            return "";
        }

        static List<Ctrl> Controls(IntPtr root)
        {
            var list = new List<Ctrl>();
            W.RECT wr; W.GetWindowRect(root, out wr);
            EnumChildWindows(root, delegate (IntPtr c, IntPtr l)
            {
                if (!W.IsWindowVisible(c)) return true;
                W.RECT r; W.GetWindowRect(c, out r);
                string cls = W.Cls(c);
                list.Add(new Ctrl { H = c, Cls = cls, Text = Clean(W.Title(c)), Kind = KindOf(c, cls), X = (r.L + r.R) / 2 - wr.L, Y = (r.T + r.B) / 2 - wr.T, Wd = r.R - r.L, Ht = r.B - r.T, Enabled = IsWindowEnabled(c) });
                return true;
            }, IntPtr.Zero);
            return list;
        }

        /// 창 안의 조작 대상 목록. 종류, 이름, 창 기준 가운데 좌표·크기
        public static string Elements(IntPtr h, int max)
        {
            IntPtr root = Guarded(h);
            var list = Controls(root);
            var sb = new StringBuilder(); int n = 0;
            foreach (var c in list)
            {
                if (n >= max) { sb.AppendFormat("(처음 {0}개만 보임)\n", max); break; }
                string kind = c.Kind.Length > 0 ? c.Kind : c.Cls;
                string text = c.Text.Length > 60 ? c.Text.Substring(0, 60) + "…" : c.Text;
                sb.AppendFormat("0x{0:X} {1} 「{2}」 ({3},{4} {5}x{6}){7}\n", (long)c.H, kind, text, c.X, c.Y, c.Wd, c.Ht, c.Enabled ? "" : " (사용 불가)");
                n++;
            }
            if (n == 0) return "Win32 자식 창이 없다(WPF·Chromium·egui 처럼 직접 그리는 앱). window_capture 로 보고 click·type·key 좌표 방식을 쓰거나, 앱 자체 자동화 통로(Office COM, 브라우저 CDP 등)를 쓴다";
            return "위치는 요소 가운데의 창 기준 좌표. 「」 안 이름으로 invoke·set_text 를 쓴다\n" + sb.ToString();
        }

        static Ctrl FindCtrl(IntPtr root, string name, int index, bool wantInput)
        {
            var list = Controls(root);
            string want = Clean(name).Trim();
            var hits = new List<Ctrl>();
            foreach (var c in list) if (c.Text.Trim() == want) hits.Add(c);
            if (hits.Count == 0) foreach (var c in list) if (c.Text.IndexOf(want, StringComparison.OrdinalIgnoreCase) >= 0) hits.Add(c);
            if (hits.Count == 0) throw new Exception("이름이 「" + name + "」 인 요소가 없다. action=elements 로 목록을 본다");
            if (index >= hits.Count) throw new Exception("「" + name + "」 요소는 " + hits.Count + "개뿐이다");
            var hit = hits[index];
            if (wantInput && hit.Kind != "입력란" && hit.Kind != "콤보")
            {
                // 이름표(글자)를 가리킨 경우 바로 다음 입력란을 쓴다(대화 상자의 이름표·입력란 순서)
                int at = list.IndexOf(hit);
                for (int i = at + 1; i < list.Count && i <= at + 3; i++) if (list[i].Kind == "입력란" || list[i].Kind == "콤보") return list[i];
                throw new Exception("「" + name + "」 다음에 입력란이 없다");
            }
            return hit;
        }

        /// 이름으로 요소를 찾아 누른다. 단추는 BM_CLICK 메시지, 그 밖은 가운데를 메시지로 클릭
        public static string Invoke(IntPtr h, string name, int index, bool pointer)
        {
            IntPtr root = Guarded(h);
            var c = FindCtrl(root, name, index, false);
            if (!c.Enabled) throw new Exception("「" + c.Text + "」 은 사용할 수 없는 상태다");
            if (pointer) Pointer(root, c.X, c.Y);
            if (c.Kind == "단추" || c.Kind == "확인란" || c.Kind == "선택 단추")
            {
                PostMessage(c.H, 0x00F5, IntPtr.Zero, IntPtr.Zero); // BM_CLICK
                if (pointer) PointerEnd(root);
                return string.Format("「{0}」 {1} 누름 (0x{2:X}, {3},{4})", c.Text, c.Kind, (long)c.H, c.X, c.Y);
            }
            return "단추가 아니어서 가운데 클릭: " + Click(root, c.X, c.Y, "left", 1, false);
        }

        /// 입력란에 값 넣기(WM_SETTEXT). 이름(입력란 앞 이름표 포함) 또는 좌표로 찾는다
        public static string SetText(IntPtr h, string name, int index, int x, int y, string text)
        {
            IntPtr root = Guarded(h);
            IntPtr t; string label;
            if (!string.IsNullOrEmpty(name)) { var c = FindCtrl(root, name, index, true); t = c.H; label = c.Text.Length > 0 ? c.Text : name; }
            else { int cx, cy; t = ChildAt(root, x, y, out cx, out cy); label = W.Cls(t); }
            string cls = W.Cls(t).ToLowerInvariant();
            if (cls == "combobox") { IntPtr ed = W.GetWindow(t, 5); if (ed != IntPtr.Zero) t = ed; } // GW_CHILD, 콤보 안 입력란
            IntPtr res;
            if (SendMessageTimeout(t, 0x000C, IntPtr.Zero, text, 2, 3000, out res) == IntPtr.Zero) throw new Exception("값을 넣지 못했다(응답 없음)");
            return string.Format("「{0}」 0x{1:X} 에 {2}자 넣음", label, (long)t, text.Length);
        }

        // ---- 주황색 가상 포인터. 대상 창 자체의 화면(DC)에 점을 그렸다가 다시 그리게 해 지운다.
        // 별도 창을 띄우지 않으므로 사용자 화면에 점이 찍힐 일이 없다(딸린 창 방식은 가상 데스크톱 배치가 보장되지 않아 버림, 11.20)
        [DllImport("user32.dll")] static extern IntPtr GetWindowDC(IntPtr h);
        [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr h, IntPtr dc);
        [DllImport("user32.dll")] static extern bool RedrawWindow(IntPtr h, IntPtr rc, IntPtr rgn, uint flags);
        static void Pointer(IntPtr root, int x, int y)
        {
            try
            {
                if (NumberOf(root) == Current()) return;
                IntPtr dc = GetWindowDC(root);
                if (dc == IntPtr.Zero) return;
                using (var g = Graphics.FromHdc(dc))
                using (var b = new SolidBrush(Color.FromArgb(255, 122, 0)))
                using (var pen = new Pen(Color.White, 2))
                {
                    g.SmoothingMode = System.Drawing.Drawing2D.SmoothingMode.AntiAlias;
                    g.FillEllipse(b, x - 9, y - 9, 18, 18);
                    g.DrawEllipse(pen, x - 9, y - 9, 18, 18);
                }
                ReleaseDC(root, dc);
                Thread.Sleep(250);
            }
            catch { }
        }

        /// 점을 지운다(창이 자기 화면을 다시 그리게 함)
        static void PointerEnd(IntPtr root)
        {
            try { Thread.Sleep(350); RedrawWindow(root, IntPtr.Zero, IntPtr.Zero, 0x0001 | 0x0004 | 0x0080 | 0x0400); } catch { } // INVALIDATE|ERASE|ALLCHILDREN|FRAME
        }

        // ---- 감시기. 격리 대상이 사용자 화면을 가져가면 즉시 되돌린다. 「Local\ClaudyDeskGuard」 로 하나만 돈다
        delegate void WinEventProc(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
        [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, WinEventProc f, int pid, int tid, uint flags);
        [DllImport("user32.dll")] static extern bool UnhookWinEvent(IntPtr h);
        [StructLayout(LayoutKind.Sequential)] struct MSG { public IntPtr hwnd; public uint message; public IntPtr wParam, lParam; public uint time; public int x, y; }
        [DllImport("user32.dll")] static extern bool PeekMessage(out MSG m, IntPtr h, uint min, uint max, uint remove);
        [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref MSG m);
        [DllImport("user32.dll")] static extern uint MsgWaitForMultipleObjects(uint n, IntPtr[] h, bool all, uint ms, uint mask);
        static WinEventProc evProc;
        [DllImport("user32.dll")] static extern short GetAsyncKeyState(int vk);
        [StructLayout(LayoutKind.Sequential)] struct LASTINPUTINFO { public int cbSize; public uint dwTime; }
        [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LASTINPUTINFO li);
        /// 마지막 사용자 입력 뒤 지난 시간(ms)
        static long IdleMs() { var li = new LASTINPUTINFO(); li.cbSize = Marshal.SizeOf(typeof(LASTINPUTINFO)); if (!GetLastInputInfo(ref li)) return long.MaxValue; return (uint)Environment.TickCount - li.dwTime; }
        static HashSet<int> gWatched = new HashSet<int>();
        static Dictionary<int, KeyValuePair<int, string>> gProcs = new Dictionary<int, KeyValuePair<int, string>>();
        static int userDesk = 0; static IntPtr userFg = IntPtr.Zero;
        public static int Blocked, Moved;

        static void Refresh() { gWatched = Watched(); gProcs = W.Procs(); }

        static void OnShow(IntPtr h)
        {
            if (W.GetAncestor(h, 2) != h || W.GetWindow(h, 4) != IntPtr.Zero) return;
            int pid = W.Pid(h);
            string name = IsolatedRoot(pid, gWatched, gProcs);
            if (name == null && !gProcs.ContainsKey(pid)) { gProcs = W.Procs(); name = IsolatedRoot(pid, gWatched, gProcs); }
            if (name == null) return;
            int n = Ensure(name);
            if (NumberOf(h) != n && Move(h, n)) { Moved++; Log("새 창을 「" + name + "」 로 옮김 pid " + pid + " " + W.Title(h)); }
            Unfold(h, n);
        }

        static void OnForeground(IntPtr h)
        {
            int pid = W.Pid(h);
            string name = IsolatedRoot(pid, gWatched, gProcs);
            if (name == null && !gProcs.ContainsKey(pid)) { gProcs = W.Procs(); name = IsolatedRoot(pid, gWatched, gProcs); }
            if (name == null)
            {
                // 모든 데스크톱에 고정된 창(사이드 패널 등, 번호 0)과 격리 데스크톱 전환 직후의 사건은 「사용자가 보던 데스크톱」을 바꾸지 않는다.
                // 사용자가 직접 다른 데스크톱으로 옮긴 경우만 반영한다(Win 키·마우스 단추가 눌린 상태)
                int hn = NumberOf(h);
                if (hn > 0)
                {
                    bool userAct = (GetAsyncKeyState(0x5B) & 0x8000) != 0 || (GetAsyncKeyState(0x5C) & 0x8000) != 0 || (GetAsyncKeyState(0x01) & 0x8000) != 0
                                   || (GetAsyncKeyState(0x11) & 0x8000) != 0 || (GetAsyncKeyState(0x12) & 0x8000) != 0 || (GetAsyncKeyState(0x09) & 0x8000) != 0;
                    if (hn == userDesk || userAct || IdleMs() < 1500) { userFg = h; userDesk = hn; }
                }
                return;
            }
            // 사용자가 직접 시험 데스크톱으로 가거나(Win+Ctrl+방향키, 작업 보기) 시험 창을 누른 경우는 막지 않는다
            if ((GetAsyncKeyState(0x5B) & 0x8000) != 0 || (GetAsyncKeyState(0x5C) & 0x8000) != 0 || (GetAsyncKeyState(0x01) & 0x8000) != 0)
            {
                Log("사용자 조작으로 격리 창이 앞에 옴(막지 않음) pid " + pid);
                return;
            }
            // 격리 대상이 전경을 가져감. 비활성 최소화로 내리고, 사용자 화면이 바뀌었으면 원래 데스크톱으로 되돌린다
            Blocked++;
            // SW_MINIMIZE 는 최소화하면서 Z 순서상 다음 창(사용자 창)을 활성화한다. SW_SHOWMINNOACTIVE(7)는 전경과 키보드 포커스를 그대로 남겨
            // 사용자의 입력이 숨은 창으로 들어가므로 쓰지 않는다
            W.ShowWindowAsync(h, 6);
            int n = Ensure(name);
            if (NumberOf(h) != n) Move(h, n);
            int now = 0; try { now = Current(); } catch { }
            string back = "";
            if (userDesk > 0 && now != userDesk && SwitchTo(userDesk)) back = ", 사용자 화면을 데스크톱 " + userDesk + " 로 되돌림";
            IntPtr prev = userFg; int wantDesk = userDesk;
            ThreadPool.QueueUserWorkItem(delegate
            {
                // 화면 전환은 전경 사건보다 늦게 끝나므로 몇 번 확인해 되돌린다
                for (int i = 0; i < 8; i++)
                {
                    Thread.Sleep(i == 0 ? 0 : 150);
                    if (W.GetForegroundWindow() == h) W.ShowWindowAsync(h, 6);
                    int c = 0; try { c = Current(); } catch { }
                    if (wantDesk > 0 && c > 0 && c != wantDesk && SwitchTo(wantDesk)) Log("사용자 화면을 데스크톱 " + wantDesk + " 로 되돌림(전환 " + c + " → " + wantDesk + ")");
                }
                // 최소화하면 Z 순서상 다음 창이 활성화되는데 사용자가 쓰던 창이 아닐 수 있다. 사용자가 쓰던 창으로 키보드 포커스를 되돌린다
                string back2 = "";
                if (prev != IntPtr.Zero && W.IsWindow(prev) && W.GetForegroundWindow() != prev)
                    back2 = RestoreUser(prev) ? "사용자 창으로 포커스 되돌림 「" + W.Title(prev) + "」" : "사용자 창으로 포커스를 되돌리지 못함";
                if (back2.Length > 0) Log(back2);
                Thread.Sleep(200);
                Unfold(h, n);
            });
            Log("격리 대상이 전경을 가져감 pid " + pid + " 「" + W.Title(h) + "」 → 비활성 최소화" + back);
        }

        /// 감시기가 없으면 띄운다. 핸들을 물려주지 않아 부른 쪽 출력이 막히지 않는다. 결과 "started"/"running"
        public static string StartGuard(string dll)
        {
            try { using (Mutex.OpenExisting("Local\\ClaudyDeskGuard")) return "running"; } catch { }
            var si = new W.STARTUPINFO(); si.cb = Marshal.SizeOf(typeof(W.STARTUPINFO));
            var cmd = new StringBuilder("powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"Add-Type -Path '" + dll + "'; [ClaudyDesk.D]::Guard()\"", 2048);
            W.PROCESS_INFORMATION pi;
            if (!W.CreateProcess(null, cmd, IntPtr.Zero, IntPtr.Zero, false, 0x08000000, IntPtr.Zero, null, ref si, out pi))
                throw new Exception("감시기를 띄우지 못했다 " + Marshal.GetLastWin32Error());
            W.CloseHandle(pi.hThread); W.CloseHandle(pi.hProcess);
            for (int i = 0; i < 50; i++) { Thread.Sleep(100); try { using (Mutex.OpenExisting("Local\\ClaudyDeskGuard")) return "started"; } catch { } }
            return "started";
        }

        [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
        [DllImport("user32.dll")] static extern bool AttachThreadInput(int a, int b, bool attach);
        [DllImport("kernel32.dll")] static extern int GetCurrentThreadId();

        /// 격리 창이 가져간 키보드 포커스를 사용자가 쓰던 창으로 되돌린다(보호 목적에 한해 씀)
        static bool RestoreUser(IntPtr u)
        {
            try
            {
                if (NumberOf(u) != Current()) return false;
                if (SetForegroundWindow(u) && W.GetForegroundWindow() == u) return true;
                IntPtr fg = W.GetForegroundWindow();
                int ft = GetWindowThreadProcessId(fg, IntPtr.Zero), me = GetCurrentThreadId();
                bool att = ft != 0 && ft != me && AttachThreadInput(me, ft, true);
                SetForegroundWindow(u);
                if (att) AttachThreadInput(me, ft, false);
                return W.GetForegroundWindow() == u;
            }
            catch { return false; }
        }

        public static void Guard()
        {
            bool created; var mtx = new Mutex(true, "Local\\ClaudyDeskGuard", out created);
            if (!created) { Console.WriteLine("감시기가 이미 돈다"); return; }
            Log("감시기 시작 pid " + Process.GetCurrentProcess().Id);
            try { userDesk = Current(); userFg = W.GetForegroundWindow(); } catch (Exception e) { Log("감시기 시작 실패 " + e.Message); return; }
            Refresh();
            evProc = delegate (IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time)
            {
                if (idObject != 0 || hwnd == IntPtr.Zero) return;
                try { if (ev == 0x0003) OnForeground(hwnd); else OnShow(hwnd); } catch (Exception e) { Log("감시기 오류 " + e.Message); }
            };
            IntPtr h1 = SetWinEventHook(0x0003, 0x0003, IntPtr.Zero, evProc, 0, 0, 0x0002); // EVENT_SYSTEM_FOREGROUND
            IntPtr h2 = SetWinEventHook(0x8002, 0x8002, IntPtr.Zero, evProc, 0, 0, 0x0002); // EVENT_OBJECT_SHOW
            DateTime lastAlive = DateTime.UtcNow;
            while (true)
            {
                MsgWaitForMultipleObjects(0, null, false, 1000, 0x04FF);
                MSG m; while (PeekMessage(out m, IntPtr.Zero, 0, 0, 1)) DispatchMessage(ref m);
                Refresh();
                // 격리 대상이 모두 끝나고 10분이 지나면 스스로 끝낸다
                bool any = false; foreach (var p in gWatched) if (gProcs.ContainsKey(p)) { any = true; break; }
                if (any) lastAlive = DateTime.UtcNow;
                if ((DateTime.UtcNow - lastAlive).TotalMinutes > 10) break;
            }
            UnhookWinEvent(h1); UnhookWinEvent(h2);
            Log("감시기 끝(격리 대상 없음 10분)");
            GC.KeepAlive(mtx);
        }
    }
}
