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
                userFg = h; try { userDesk = Current(); } catch { }
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
            W.ShowWindowAsync(h, 7);
            int n = Ensure(name);
            if (NumberOf(h) != n) Move(h, n);
            int now = 0; try { now = Current(); } catch { }
            string back = "";
            if (userDesk > 0 && now != userDesk && SwitchTo(userDesk)) back = ", 사용자 화면을 데스크톱 " + userDesk + " 로 되돌림";
            ThreadPool.QueueUserWorkItem(delegate { Thread.Sleep(400); Unfold(h, n); });
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

        public static void Guard()
        {
            bool created; var mtx = new Mutex(true, "Local\\ClaudyDeskGuard", out created);
            if (!created) { Console.WriteLine("감시기가 이미 돈다"); return; }
            Log("감시기 시작 pid " + Process.GetCurrentProcess().Id);
            try { userDesk = Current(); } catch (Exception e) { Log("감시기 시작 실패 " + e.Message); return; }
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
