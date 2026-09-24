// Test-only Windows desktop/console adapter. No calls occur when this file is compiled.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace HerdrInputGauntlet {
    [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
    class ApplicationActivationManager { }

    [ComImport, Guid("2E941141-7F97-4756-BA1D-9DECDE894A3D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationActivationManager {
        [PreserveSig]
        int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
            [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
    }

    public static class Desktop {
        [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
        [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
        [StructLayout(LayoutKind.Sequential)] struct Mouse { public int X, Y; public uint Data, Flags, Time; public UIntPtr Extra; }
        [StructLayout(LayoutKind.Sequential)] struct Key { public ushort Vk, Scan; public uint Flags, Time; public UIntPtr Extra; }
        [StructLayout(LayoutKind.Explicit)] struct Union { [FieldOffset(0)] public Mouse Mouse; [FieldOffset(0)] public Key Key; }
        [StructLayout(LayoutKind.Sequential)] struct Input { public uint Type; public Union Data; }
        delegate bool EnumProc(IntPtr hwnd, IntPtr arg);
        delegate IntPtr HookProc(int code,IntPtr message,IntPtr data);
        [StructLayout(LayoutKind.Sequential)] struct Message { public IntPtr Window; public uint Id; public UIntPtr WParam; public IntPtr LParam; public uint Time; public Point Point; public uint Private; }
        [DllImport("user32.dll",SetLastError=true)] static extern IntPtr SetWindowsHookEx(int kind,HookProc callback,IntPtr module,uint thread);
        [DllImport("user32.dll")] static extern bool UnhookWindowsHookEx(IntPtr hook);
        [DllImport("user32.dll")] static extern IntPtr CallNextHookEx(IntPtr hook,int code,IntPtr message,IntPtr data);
        [DllImport("user32.dll")] static extern int GetMessage(out Message message,IntPtr window,uint min,uint max);
        [DllImport("user32.dll")] static extern bool PostThreadMessage(uint thread,uint message,UIntPtr wparam,IntPtr lparam);
        [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
        [DllImport("kernel32.dll",CharSet=CharSet.Unicode)] static extern IntPtr GetModuleHandle(string name);
        static volatile bool stopped;
        static Thread stopThread;
        static uint stopThreadId;
        static HookProc stopHook;
        static readonly ManualResetEventSlim stopReady=new ManualResetEventSlim();
        static string stopError;
        public static void StartEmergencyStop() {
            stopThread=new Thread(()=> {
                IntPtr hook=IntPtr.Zero;
                try {
                    stopThreadId=GetCurrentThreadId();
                    stopHook=(code,message,data)=> {
                        if(code>=0 && (message.ToInt64()==0x100 || message.ToInt64()==0x104) && Marshal.ReadInt32(data)==0x7B) stopped=true;
                        return CallNextHookEx(IntPtr.Zero,code,message,data);
                    };
                    hook=SetWindowsHookEx(13,stopHook,GetModuleHandle(null),0);
                    if(hook==IntPtr.Zero) throw new Win32Exception();
                    stopReady.Set();
                    Message message;
                    while(GetMessage(out message,IntPtr.Zero,0,0)>0) {}
                } catch(Exception e) { stopError=e.Message; stopped=true; stopReady.Set(); }
                finally { if(hook!=IntPtr.Zero) UnhookWindowsHookEx(hook); }
            }) { IsBackground=true };
            stopThread.Start();
            if(!stopReady.Wait(3000) || stopError!=null) throw new Exception("Cannot install emergency-stop latch: "+stopError);
        }
        public static void StopEmergencyStop() {
            if(stopThread!=null) { PostThreadMessage(stopThreadId,0x12,UIntPtr.Zero,IntPtr.Zero); stopThread.Join(1000); }
        }
        [DllImport("kernel32.dll",SetLastError=true)] static extern IntPtr OpenProcess(uint access,bool inherit,int pid);
        [DllImport("advapi32.dll",SetLastError=true)] static extern bool OpenProcessToken(IntPtr process,uint access,out IntPtr token);
        [DllImport("advapi32.dll",SetLastError=true)] static extern bool GetTokenInformation(IntPtr token,int kind,out uint value,int size,out int returned);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        [StructLayout(LayoutKind.Sequential)] struct FileInformation {
            public uint Attributes,CreationLow,CreationHigh,AccessLow,AccessHigh,WriteLow,WriteHigh;
            public uint Volume,SizeHigh,SizeLow,Links,IndexHigh,IndexLow;
        }
        [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern Microsoft.Win32.SafeHandles.SafeFileHandle CreateFile(string path,uint access,uint share,IntPtr security,uint creation,uint flags,IntPtr template);
        [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetFileInformationByHandle(Microsoft.Win32.SafeHandles.SafeFileHandle file,out FileInformation info);
        public static string FileIdentity(string path) {
            using(var file=CreateFile(path,0,7,IntPtr.Zero,3,0x02000000,IntPtr.Zero)) {
                FileInformation info;
                if(file.IsInvalid || !GetFileInformationByHandle(file,out info)) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot establish Terminal file/installation identity");
                return info.Volume.ToString("x8")+":"+info.IndexHigh.ToString("x8")+info.IndexLow.ToString("x8");
            }
        }
        public static void AssertNotElevated(int pid) {
            var process=OpenProcess(0x1000,false,pid);
            if(process==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot verify process elevation; refusing desktop input");
            IntPtr token=IntPtr.Zero;
            try {
                uint elevated; int returned;
                if(!OpenProcessToken(process,8,out token) || !GetTokenInformation(token,20,out elevated,4,out returned) || returned!=4)
                    throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot verify process elevation; refusing desktop input");
                if(elevated!=0) throw new Exception("Elevated controller/Terminal is not allowed. Start PowerShell and Terminal without Run as administrator.");
            } finally { if(token!=IntPtr.Zero) CloseHandle(token); CloseHandle(process); }
        }
        [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr arg);
        [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder value, int count);
        [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
        [DllImport("user32.dll")] static extern IntPtr GetKeyboardLayout(uint thread);
        [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
        [DllImport("user32.dll")] static extern bool GetCursorPos(out Point point);
        [DllImport("user32.dll")] static extern bool SetCursorPos(int x,int y);
        [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
        [DllImport("user32.dll")] static extern short GetAsyncKeyState(int vk);
        [DllImport("user32.dll")] static extern uint MapVirtualKeyEx(uint key, uint kind, IntPtr layout);
        [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int ToUnicodeEx(uint key,uint scan,byte[] state,StringBuilder text,int count,uint flags,IntPtr layout);
        [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint count, Input[] events, int size);
        [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
        [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int width, int height, uint flags);
        [DllImport("user32.dll",EntryPoint="CountClipboardFormats",SetLastError=true)] static extern int NativeCountClipboardFormats();
        public static int CountClipboardFormats() {
            int count=NativeCountClipboardFormats();
            if(count==0) {
                int error=Marshal.GetLastWin32Error();
                if(error!=0) throw new Win32Exception(error,"Clipboard format count failed");
            }
            return count;
        }
        [DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
        [DllImport("user32.dll")] static extern IntPtr GetClipboardOwner();
        [DllImport("user32.dll")] static extern bool OpenClipboard(IntPtr owner);
        [DllImport("user32.dll")] static extern bool CloseClipboard();
        [DllImport("user32.dll")] static extern bool EmptyClipboard();
        [DllImport("user32.dll")] static extern IntPtr SetClipboardData(uint format,IntPtr value);
        [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern uint RegisterClipboardFormat(string format);
        [DllImport("kernel32.dll")] static extern IntPtr GlobalAlloc(uint flags,UIntPtr size);
        [DllImport("kernel32.dll")] static extern IntPtr GlobalLock(IntPtr memory);
        [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr memory);
        [DllImport("kernel32.dll")] static extern IntPtr GlobalFree(IntPtr memory);

        static string QuoteArgument(string value) {
            if(value.Length>0 && value.IndexOfAny(new[]{' ', '\t', '\n', '\v', '"'})<0) return value;
            var result=new StringBuilder("\"");
            int slashes=0;
            foreach(char c in value) {
                if(c=='\\') { slashes++; continue; }
                if(c=='"') result.Append('\\',slashes*2+1);
                else result.Append('\\',slashes);
                result.Append(c); slashes=0;
            }
            result.Append('\\',slashes*2).Append('"');
            return result.ToString();
        }
        public static int ActivateApplication(string appUserModelId,string[] arguments) {
            var manager=(IApplicationActivationManager)new ApplicationActivationManager();
            uint pid;
            int result=manager.ActivateApplication(appUserModelId,string.Join(" ",Array.ConvertAll(arguments,QuoteArgument)),0,out pid);
            if(result<0) Marshal.ThrowExceptionForHR(result);
            return checked((int)pid);
        }
        public static int ClearClipboardForRun() {
            if(!OpenClipboard(IntPtr.Zero)) throw new Exception("Clipboard busy; cannot start paste qualification");
            try {
                int formats=CountClipboardFormats();
                if(formats!=0 && !EmptyClipboard()) throw new Exception("Clipboard clear failed");
                return formats;
            } finally { CloseClipboard(); }
        }

        public static uint SetEmptyClipboard(IntPtr owner,string text) {
            if(owner==IntPtr.Zero) throw new Exception("Clipboard owner is required");
            if(!OpenClipboard(owner)) throw new Exception("Clipboard busy; refusing replacement");
            IntPtr memory=IntPtr.Zero;
            try {
                if(CountClipboardFormats()!=0) throw new Exception("Clipboard changed or contains user data; refusing replacement");
                byte[] data=Encoding.Unicode.GetBytes(text+"\0");
                memory=GlobalAlloc(2,new UIntPtr((uint)data.Length));
                if(memory==IntPtr.Zero) throw new Exception("Clipboard allocation failed");
                var pointer=GlobalLock(memory);
                if(pointer==IntPtr.Zero) throw new Exception("Clipboard lock failed");
                try { Marshal.Copy(data,0,pointer,data.Length); } finally { GlobalUnlock(memory); }
                if(!EmptyClipboard() || SetClipboardData(13,memory)==IntPtr.Zero) throw new Exception("Clipboard write failed");
                memory=IntPtr.Zero; // ownership transferred to Windows
            } finally { if(memory!=IntPtr.Zero) GlobalFree(memory); CloseClipboard(); }
            return AdoptClipboardLease(owner);
        }
        public static uint SetEmptyClipboardImage(IntPtr owner,string text) {
            if(owner==IntPtr.Zero) throw new Exception("Clipboard owner is required");
            if(!OpenClipboard(owner)) throw new Exception("Clipboard busy; refusing replacement");
            IntPtr pngMemory=IntPtr.Zero,textMemory=IntPtr.Zero;
            bool complete=false;
            try {
                if(CountClipboardFormats()!=0) throw new Exception("Clipboard changed or contains user data; refusing replacement");
                byte[] png=Convert.FromBase64String("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAAXNSR0IArs4c6QAAAARnQU1BAACxjwv8YQUAAAAJcEhZcwAADsMAAA7DAcdvqGQAAAANSURBVBhXY/jPwPAfAAUAAf+mXJtdAAAAAElFTkSuQmCC");
                pngMemory=GlobalAlloc(2,new UIntPtr((uint)png.Length));
                if(pngMemory==IntPtr.Zero) throw new Exception("Clipboard allocation failed");
                var pointer=GlobalLock(pngMemory);
                if(pointer==IntPtr.Zero) throw new Exception("Clipboard allocation failed");
                try { Marshal.Copy(png,0,pointer,png.Length); } finally { GlobalUnlock(pngMemory); }
                if(!EmptyClipboard()) throw new Exception("Clipboard clear failed");
                uint pngFormat=RegisterClipboardFormat("PNG");
                if(pngFormat==0 || SetClipboardData(pngFormat,pngMemory)==IntPtr.Zero) throw new Exception("Clipboard image write failed");
                pngMemory=IntPtr.Zero;
                if(text!=null) {
                    byte[] data=Encoding.Unicode.GetBytes(text+"\0");
                    textMemory=GlobalAlloc(2,new UIntPtr((uint)data.Length));
                    if(textMemory==IntPtr.Zero) throw new Exception("Clipboard allocation failed");
                    pointer=GlobalLock(textMemory);
                    if(pointer==IntPtr.Zero) throw new Exception("Clipboard allocation failed");
                    try { Marshal.Copy(data,0,pointer,data.Length); } finally { GlobalUnlock(textMemory); }
                    if(SetClipboardData(13,textMemory)==IntPtr.Zero) throw new Exception("Clipboard text write failed");
                    textMemory=IntPtr.Zero;
                }
                complete=true;
            } finally {
                if(!complete) EmptyClipboard();
                if(pngMemory!=IntPtr.Zero) GlobalFree(pngMemory);
                if(textMemory!=IntPtr.Zero) GlobalFree(textMemory);
                CloseClipboard();
            }
            return AdoptClipboardLease(owner);
        }
        static uint AdoptClipboardLease(IntPtr owner) {
            // Closing can synthesize additional formats and advance the sequence.
            uint sequence=GetClipboardSequenceNumber();
            try {
                if(!OpenClipboard(owner)) throw new Exception("Clipboard busy; cannot establish test ownership");
                try {
                    if(GetClipboardOwner()!=owner || GetClipboardSequenceNumber()!=sequence)
                        throw new Exception("Clipboard ownership changed; refusing cleanup lease");
                    return sequence;
                } finally { CloseClipboard(); }
            } catch(Exception error) {
                if(!ClearOwnedClipboard(owner,sequence))
                    throw new Exception("Could not clean clipboard after ownership verification failed",error);
                throw;
            }
        }
        public static bool ClearOwnedClipboard(IntPtr owner,uint sequence) {
            if(owner==IntPtr.Zero) return false;
            if(!OpenClipboard(IntPtr.Zero)) return false;
            try { return GetClipboardSequenceNumber()!=sequence || GetClipboardOwner()!=owner || EmptyClipboard(); }
            finally { CloseClipboard(); }
        }
        public static string Title(IntPtr hwnd) { var text=new StringBuilder(1024); GetWindowText(hwnd,text,text.Capacity); return text.ToString(); }
        public static IntPtr Find(string nonce) {
            var matches=new List<IntPtr>();
            EnumWindows((h,a)=> { if (Title(h).Contains(nonce)) matches.Add(h); return true; },IntPtr.Zero);
            if(matches.Count>1) throw new Exception("Ambiguous test window identity");
            return matches.Count==1 ? matches[0] : IntPtr.Zero;
        }
        public static int Pid(IntPtr hwnd) { uint pid; GetWindowThreadProcessId(hwnd,out pid); return checked((int)pid); }
        public static string Layout(IntPtr hwnd) { uint pid; return GetKeyboardLayout(GetWindowThreadProcessId(hwnd,out pid)).ToInt64().ToString("x"); }
        public static bool IsOwned(IntPtr hwnd, string nonce, int pid) { return hwnd!=IntPtr.Zero && Pid(hwnd)==pid && Title(hwnd).Contains(nonce); }
        public static void Guard(IntPtr hwnd, string nonce, int pid) {
            if(!IsOwned(hwnd,nonce,pid) || GetForegroundWindow()!=hwnd) throw new Exception("Lost owned test-window focus; injection aborted");
            AssertNotElevated(pid);
            if(stopped || (GetAsyncKeyState(0x7B)&0x8000)!=0) throw new Exception("F12 emergency stop");
        }
        public static void Focus(IntPtr hwnd, string nonce, int pid) {
            if(!IsOwned(hwnd,nonce,pid)) throw new Exception("Lost window ownership");
            AssertNotElevated(pid);
            var deadline=DateTime.UtcNow.AddSeconds(3);
            do {
                SetForegroundWindow(hwnd);
                if(GetForegroundWindow()==hwnd) return;
                Thread.Sleep(100);
            } while(DateTime.UtcNow<deadline);
            throw new Exception("Cannot focus test window; no input sent");
        }
        public static void FocusAwayAndBack(IntPtr other,IntPtr hwnd,string nonce,int pid) {
            if(other==IntPtr.Zero || other==hwnd) throw new Exception("No separate controller window for focus-cycle qualification");
            AssertNotElevated(Pid(other));
            var deadline=DateTime.UtcNow.AddSeconds(3);
            do {
                SetForegroundWindow(other);
                if(GetForegroundWindow()==other) break;
                Thread.Sleep(100);
            } while(DateTime.UtcNow<deadline);
            if(GetForegroundWindow()!=other) throw new Exception("Cannot focus controller window for focus-cycle qualification");
            Thread.Sleep(250);
            Focus(hwnd,nonce,pid);
            Thread.Sleep(250);
        }
        public static void Neutral() {
            foreach(int key in new[]{0x10,0x11,0x12,0x5B,0x5C,1,2,4})
                if((GetAsyncKeyState(key)&0x8000)!=0) throw new Exception("Release physical modifiers and mouse buttons before running");
        }
        static Input Event(ushort vk, bool release, IntPtr layout) {
            uint mapped=MapVirtualKeyEx(vk,4,layout);
            if(mapped==0) throw new Exception("No scan code for virtual key "+vk);
            return new Input { Type=1, Data=new Union { Key=new Key { Scan=(ushort)(mapped&255), Flags=8u|(release?2u:0u)|((mapped&0xff00)!=0?1u:0u) } } };
        }
        public static int[] Scans(IntPtr hwnd,int[] keys) {
            uint ignored; var layout=GetKeyboardLayout(GetWindowThreadProcessId(hwnd,out ignored));
            var scans=new List<int>();
            foreach(int key in keys) scans.Add(Event((ushort)key,false,layout).Data.Key.Scan);
            return scans.ToArray();
        }
        static int[] LayoutChord(int vk,int modifiers) {
            var keys=new List<int>();
            if((modifiers&1)!=0) keys.Add(0x10);
            if((modifiers&2)!=0) keys.Add(0x11);
            if((modifiers&4)!=0) keys.Add(0x12);
            keys.Add(vk);
            return keys.ToArray();
        }
        public static int[] DeadKeyChord(IntPtr hwnd,char accent) {
            uint ignored; var layout=GetKeyboardLayout(GetWindowThreadProcessId(hwnd,out ignored));
            for(int modifiers=0;modifiers<8;modifiers++) for(uint vk=1;vk<255;vk++) {
                uint scan=MapVirtualKeyEx(vk,4,layout);
                if(scan==0) continue;
                var state=new byte[256];
                if((modifiers&1)!=0) state[0x10]=0x80;
                if((modifiers&2)!=0) state[0x11]=0x80;
                if((modifiers&4)!=0) state[0x12]=0x80;
                var text=new StringBuilder(4);
                // Flag 4 discovers dead keys without changing keyboard state.
                if(ToUnicodeEx(vk,scan,state,text,text.Capacity,4,layout)<0 && text.Length>0 && text[0]==accent)
                    return LayoutChord((int)vk,modifiers);
            }
            throw new Exception("Active layout has no "+accent+" dead key");
        }
        // One balanced batch; no modifier is intentionally held between calls.
        public static int Chord(IntPtr hwnd, string nonce, int pid, int[] keys) {
            Guard(hwnd,nonce,pid); Neutral();
            uint ignored; var layout=GetKeyboardLayout(GetWindowThreadProcessId(hwnd,out ignored));
            var events=new List<Input>();
            foreach(int key in keys) events.Add(Event(checked((ushort)key),false,layout));
            for(int i=keys.Length-1;i>=0;i--) events.Add(Event(checked((ushort)keys[i]),true,layout));
            Guard(hwnd,nonce,pid);
            uint sent=SendInput((uint)events.Count,events.ToArray(),Marshal.SizeOf<Input>());
            if(sent!=events.Count) {
                // Release only keys whose down event was actually submitted and not yet released.
                var held=new List<Input>();
                for(int i=0;i<keys.Length && i<sent;i++)
                    if(sent <= 2*keys.Length-1-i) held.Add(Event((ushort)keys[i],true,layout));
                if(held.Count>0) {
                    uint released=SendInput((uint)held.Count,held.ToArray(),Marshal.SizeOf<Input>());
                    if(released!=held.Count) throw new Exception("SendInput cleanup incomplete: "+released+"/"+held.Count+" key-up events; release test modifiers manually before continuing");
                }
                throw new Exception("SendInput incomplete (possible UIPI restriction): "+sent+"/"+events.Count);
            }
            return (int)sent;
        }
        public static long Cursor() {
            Point point;
            if(!GetCursorPos(out point)) throw new Win32Exception();
            return ((long)(uint)point.X<<32)|(uint)point.Y;
        }
        public static void RestoreCursor(long packed) {
            if(!SetCursorPos(unchecked((int)(packed>>32)),unchecked((int)packed))) throw new Win32Exception();
        }
        public static void MouseMoveInside(IntPtr hwnd,string nonce,int pid,int offset) {
            Guard(hwnd,nonce,pid); Neutral(); Rect rect;
            if(!GetWindowRect(hwnd,out rect)) throw new Win32Exception();
            int x=(rect.Left+rect.Right)/2+offset,y=(rect.Top+rect.Bottom)/2;
            if(x<=rect.Left+20 || x>=rect.Right-20 || y<=rect.Top+20 || y>=rect.Bottom-20) throw new Exception("Mouse target is outside the safe window interior");
            int left=GetSystemMetrics(76),top=GetSystemMetrics(77),width=GetSystemMetrics(78),height=GetSystemMetrics(79);
            if(width<2 || height<2) throw new Exception("Virtual desktop geometry unavailable");
            int nx=(int)Math.Round((x-left)*65535.0/(width-1)),ny=(int)Math.Round((y-top)*65535.0/(height-1));
            var input=new Input { Type=0, Data=new Union { Mouse=new Mouse { X=nx,Y=ny,Flags=0xC001 } } };
            if(SendInput(1,new[]{input},Marshal.SizeOf<Input>())!=1) throw new Win32Exception(Marshal.GetLastWin32Error(),"Mouse injection failed");
        }
        public static void MouseClickWheelInside(IntPtr hwnd,string nonce,int pid,int offset) {
            Guard(hwnd,nonce,pid); Neutral(); Rect rect;
            if(!GetWindowRect(hwnd,out rect)) throw new Win32Exception();
            int x=(rect.Left+rect.Right)/2+offset,y=(rect.Top+rect.Bottom)/2;
            if(x<=rect.Left+20 || x>=rect.Right-20 || y<=rect.Top+20 || y>=rect.Bottom-20) throw new Exception("Mouse target is outside the safe window interior");
            int left=GetSystemMetrics(76),top=GetSystemMetrics(77),width=GetSystemMetrics(78),height=GetSystemMetrics(79);
            if(width<2 || height<2) throw new Exception("Virtual desktop geometry unavailable");
            int nx=(int)Math.Round((x-left)*65535.0/(width-1)),ny=(int)Math.Round((y-top)*65535.0/(height-1));
            var inputs=new[]{
                new Input { Type=0, Data=new Union { Mouse=new Mouse { X=nx,Y=ny,Flags=0xC001 } } },
                new Input { Type=0, Data=new Union { Mouse=new Mouse { Flags=0x0002 } } },
                new Input { Type=0, Data=new Union { Mouse=new Mouse { Flags=0x0004 } } },
                new Input { Type=0, Data=new Union { Mouse=new Mouse { Data=120,Flags=0x0800 } } }
            };
            uint sent=SendInput((uint)inputs.Length,inputs,Marshal.SizeOf<Input>());
            if(sent!=inputs.Length) {
                int error=Marshal.GetLastWin32Error();
                if(sent==2 && SendInput(1,new[]{inputs[2]},Marshal.SizeOf<Input>())!=1)
                    throw new Win32Exception(Marshal.GetLastWin32Error(),"Mouse injection failed after button-down; release the left mouse button manually");
                throw new Win32Exception(error,"Mouse click/wheel injection failed");
            }
        }
        public static void Resize(IntPtr hwnd,string nonce,int pid,int dx,int dy) {
            Guard(hwnd,nonce,pid); Rect r;
            if(!GetWindowRect(hwnd,out r)) throw new Win32Exception();
            int w=r.Right-r.Left+dx,h=r.Bottom-r.Top+dy;
            if(w<300 || h<200 || w>10000 || h>10000) throw new Exception("Unsafe resize request");
            if(!SetWindowPos(hwnd,IntPtr.Zero,0,0,w,h,0x16)) throw new Win32Exception();
        }
    }

    public sealed class ConsoleProbe : IDisposable {
        [StructLayout(LayoutKind.Sequential)] struct Coord { public short X,Y; }
        [StructLayout(LayoutKind.Sequential)] struct SmallRect { public short Left,Top,Right,Bottom; }
        [StructLayout(LayoutKind.Sequential)] struct Info { public Coord Size,Cursor; public ushort Attributes; public SmallRect Window; public Coord Maximum; }
        [StructLayout(LayoutKind.Explicit,Size=20)] struct Record {
            [FieldOffset(0)] public ushort Type;
            [FieldOffset(4)] public int Down;
            [FieldOffset(8)] public ushort Repeat;
            [FieldOffset(10)] public ushort Vk;
            [FieldOffset(12)] public ushort Scan;
            [FieldOffset(14)] public ushort Unicode;
            [FieldOffset(16)] public uint Control;
        }
        [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int kind);
        [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetConsoleMode(IntPtr handle,out uint mode);
        [DllImport("kernel32.dll",SetLastError=true)] static extern bool SetConsoleMode(IntPtr handle,uint mode);
        [DllImport("kernel32.dll")] static extern bool GetConsoleScreenBufferInfo(IntPtr handle,out Info info);
        [DllImport("kernel32.dll")] static extern uint GetConsoleCP();
        [DllImport("kernel32.dll")] static extern bool SetConsoleCP(uint cp);
        [DllImport("kernel32.dll")] static extern bool ReadFile(IntPtr handle,byte[] buffer,uint size,out uint count,IntPtr overlap);
        [DllImport("kernel32.dll")] static extern bool WriteFile(IntPtr handle,byte[] buffer,uint size,out uint count,IntPtr overlap);
        [DllImport("kernel32.dll")] static extern bool ReadConsoleInputW(IntPtr handle,[Out] Record[] records,uint size,out uint count);
        [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
        [DllImport("kernel32.dll")] static extern IntPtr OpenThread(uint access,bool inherit,uint id);
        [DllImport("kernel32.dll")] static extern bool CancelSynchronousIo(IntPtr thread);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        readonly object gate=new object();
        readonly List<byte> bytes=new List<byte>();
        readonly List<long[]> records=new List<long[]>();
        readonly IntPtr input=GetStdHandle(-10), output=GetStdHandle(-11);
        readonly uint original, outputMode, cp;
        readonly bool native;
        volatile bool running=true;
        Thread reader;
        IntPtr threadHandle;
        string error;
        public string Error {
            get { return Volatile.Read(ref error); }
            private set { Volatile.Write(ref error,value); }
        }
        public ConsoleProbe(string mode) {
            if(!GetConsoleMode(input,out original) || !GetConsoleMode(output,out outputMode)) throw new Exception("Probe needs a real console");
            cp=GetConsoleCP(); native=mode=="native";
            try {
                if(!SetConsoleCP(65001) || !SetConsoleMode(output,outputMode|4) || !SetConsoleMode(input,native ? (original & ~0x247u)|0x98u : (original & ~7u)|0x200u)) throw new Win32Exception();
                Print("\x1b[?2004h"+(mode=="kitty"?"\x1b[>1u\x1b[?u":mode=="mok2"?"\x1b[>4;2m":""));
                reader=new Thread(Read) { IsBackground=true }; reader.Start();
            } catch { SetConsoleMode(input,original); SetConsoleMode(output,outputMode); SetConsoleCP(cp); throw; }
        }
        public static long[] Geometry() {
            Info info; uint mode;
            if(!GetConsoleScreenBufferInfo(GetStdHandle(-11),out info) || !GetConsoleMode(GetStdHandle(-10),out mode)) throw new Exception("Console geometry unavailable");
            return new long[]{ info.Window.Right-info.Window.Left+1,info.Window.Bottom-info.Window.Top+1,mode };
        }
        public static void Print(string text) {
            byte[] data=Encoding.UTF8.GetBytes(text); uint count;
            if(!WriteFile(GetStdHandle(-11),data,(uint)data.Length,out count,IntPtr.Zero) || count!=data.Length) throw new Exception("Console output failed");
        }
        public void SetKeyboardMode(string mode) {
            string sequence="\x1b[<u\x1b[>4;0m";
            if(mode=="mok2") sequence+="\x1b[>4;2m";
            else if(mode=="kitty") sequence+="\x1b[>1u";
            else if(mode!="legacy") throw new Exception("Unknown keyboard mode transition");
            Print(sequence);
        }
        void Read() {
            threadHandle=OpenThread(1,false,GetCurrentThreadId());
            if(threadHandle==IntPtr.Zero) { Error="Cannot acquire reader cancellation handle"; return; }
            try {
                while(running) {
                    if(Count()>1048576) { Error="Probe input limit exceeded"; break; }
                    uint count;
                    if(native) {
                        var batch=new Record[64];
                        if(!ReadConsoleInputW(input,batch,64,out count)) { if(running) Error="ReadConsoleInputW failed"; break; }
                        lock(gate) for(int i=0;i<count;i++) {
                            var r=batch[i];
                            // Preserve every native record, including non-key event type and payload.
                            records.Add(new long[]{r.Type,r.Down,r.Repeat,r.Vk,r.Scan,r.Unicode,r.Control});
                        }
                    } else {
                        var batch=new byte[4096];
                        if(!ReadFile(input,batch,4096,out count,IntPtr.Zero)) { if(running) Error="ReadFile failed"; break; }
                        lock(gate) for(int i=0;i<count;i++) bytes.Add(batch[i]);
                    }
                }
            } catch(Exception e) { Error=e.Message; }
        }
        public void Clear() { lock(gate) { bytes.Clear(); records.Clear(); } }
        public bool ClearIfCount(int expected) {
            lock(gate) {
                if(bytes.Count+records.Count!=expected) {
                    if(!native || bytes.Count!=0 || records.Count<expected) return false;
                    for(int i=expected;i<records.Count;i++)
                        if(records[i][0]!=4 && records[i][0]!=16) return false; // resize/focus between captures
                }
                bytes.Clear(); records.Clear(); return true;
            }
        }
        public string Hex() { lock(gate) return BitConverter.ToString(bytes.ToArray()).Replace("-","").ToLowerInvariant(); }
        public long[][] Records() { lock(gate) return records.ToArray(); }
        public int Count() { lock(gate) return bytes.Count+records.Count; }
        public string[] CleanupErrors { get; private set; } = new string[0];
        public void Dispose() {
            var errors=new List<string>();
            running=false;
            if(threadHandle!=IntPtr.Zero) CancelSynchronousIo(threadHandle);
            if(reader!=null && !reader.Join(1000)) errors.Add("Reader did not stop");
            if(threadHandle!=IntPtr.Zero) CloseHandle(threadHandle);
            try { Print("\x1b[<u\x1b[>4;0m\x1b[?2004l\x1b[?1000l\x1b[?1006l"); }
            catch(Exception e) { errors.Add(e.Message); }
            if(!SetConsoleMode(input,original)) errors.Add("Input-mode restoration failed");
            if(!SetConsoleMode(output,outputMode)) errors.Add("Output-mode restoration failed");
            if(!SetConsoleCP(cp)) errors.Add("Code-page restoration failed");
            uint actual;
            if(!GetConsoleMode(input,out actual) || actual!=original) errors.Add("Input mode differs after restoration");
            if(!GetConsoleMode(output,out actual) || actual!=outputMode) errors.Add("Output mode differs after restoration");
            if(GetConsoleCP()!=cp) errors.Add("Code page differs after restoration");
            CleanupErrors=errors.ToArray();
        }
    }
}
