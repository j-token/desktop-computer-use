#include <chrono>
#include <cmath>
#include <fstream>
#include <iostream>
#include <string>

namespace {
std::ofstream logFile;
const auto origin = std::chrono::steady_clock::now();
bool held = false;
double startX = 0, startY = 0, pointerX = 0, pointerY = 0;
int moves = 0, completed = 0;
void record(const char *event, double x, double y, bool button) {
  auto micros = std::chrono::duration_cast<std::chrono::microseconds>(
                    std::chrono::steady_clock::now() - origin)
                    .count();
  logFile << "{\"event\":\"" << event << "\",\"us\":" << micros
          << ",\"x\":" << x << ",\"y\":" << y
          << ",\"left\":" << (button ? "true" : "false") << "}\n"
          << std::flush;
  pointerX = x;
  pointerY = y;
  if (std::string(event) == "down") {
    held = true;
    startX = x;
    startY = y;
    moves = 0;
  }
  if (std::string(event) == "move" && held && button)
    ++moves;
  if (std::string(event) == "up") {
    if (held && moves >= 2 && std::hypot(x - startX, y - startY) > 40) {
      ++completed;
      logFile << "{\"event\":\"drag-complete\",\"us\":" << micros
              << ",\"moves\":" << moves << ",\"count\":" << completed << "}\n"
              << std::flush;
    }
    held = false;
  }
}
} // namespace
#ifdef _WIN32
#include <windows.h>
#include <windowsx.h>
LRESULT CALLBACK windowProc(HWND hwnd, UINT msg, WPARAM w, LPARAM l) {
  switch (msg) {
  case WM_LBUTTONDOWN:
    SetCapture(hwnd);
    record("down", GET_X_LPARAM(l), GET_Y_LPARAM(l), true);
    InvalidateRect(hwnd, nullptr, FALSE);
    return 0;
  case WM_MOUSEMOVE:
    record("move", GET_X_LPARAM(l), GET_Y_LPARAM(l), (w & MK_LBUTTON) != 0);
    InvalidateRect(hwnd, nullptr, FALSE);
    return 0;
  case WM_LBUTTONUP:
    record("up", GET_X_LPARAM(l), GET_Y_LPARAM(l), false);
    ReleaseCapture();
    InvalidateRect(hwnd, nullptr, FALSE);
    return 0;
  case WM_KEYDOWN:
    if (w == VK_ESCAPE)
      DestroyWindow(hwnd);
    return 0;
  case WM_PAINT: {
    PAINTSTRUCT ps;
    HDC dc = BeginPaint(hwnd, &ps);
    RECT r;
    GetClientRect(hwnd, &r);
    FillRect(dc, &r, static_cast<HBRUSH>(GetStockObject(WHITE_BRUSH)));
    SetBkMode(dc, TRANSPARENT);
    const std::wstring title = L"DCU native drag fixture | drag blue square to "
                               L"green square | Esc closes";
    TextOutW(dc, 20, 20, title.c_str(), static_cast<int>(title.size()));
    HBRUSH blue = CreateSolidBrush(RGB(45, 110, 230)),
           green = CreateSolidBrush(RGB(35, 180, 105));
    RECT a{80, 120, 200, 240}, b{480, 120, 600, 240};
    FillRect(dc, &a, blue);
    FillRect(dc, &b, green);
    DeleteObject(blue);
    DeleteObject(green);
    const auto status = L"Completed drags: " + std::to_wstring(completed) +
                        L" | held: " + std::to_wstring(held) + L" | moves: " +
                        std::to_wstring(moves);
    TextOutW(dc, 20, 300, status.c_str(), static_cast<int>(status.size()));
    if (held) {
      MoveToEx(dc, static_cast<int>(startX), static_cast<int>(startY), nullptr);
      LineTo(dc, static_cast<int>(pointerX), static_cast<int>(pointerY));
    }
    EndPaint(hwnd, &ps);
    return 0;
  }
  case WM_DESTROY:
    record("closed", pointerX, pointerY, held);
    PostQuitMessage(0);
    return 0;
  }
  return DefWindowProcW(hwnd, msg, w, l);
}
int main(int argc, char **argv) {
  logFile.open(argc > 1 ? argv[1] : "drag-events.ndjson", std::ios::trunc);
  if (!logFile) {
    std::cerr << "Cannot open event log\n";
    return 1;
  }
  SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
  HINSTANCE instance = GetModuleHandleW(nullptr);
  WNDCLASSW klass{};
  klass.lpfnWndProc = windowProc;
  klass.hInstance = instance;
  klass.lpszClassName = L"DCUDragFixture";
  klass.hCursor = LoadCursorW(nullptr, IDC_ARROW);
  if (!RegisterClassW(&klass))
    return 2;
  HWND window = CreateWindowExW(
      0, klass.lpszClassName, L"DCU native drag fixture", WS_OVERLAPPEDWINDOW,
      100, 100, 760, 480, nullptr, nullptr, instance, nullptr);
  if (!window)
    return 3;
  // Start-Process -WindowStyle Hidden suppresses the console; its startup flag
  // overrides the first ShowWindow call, so explicitly show the fixture next.
  ShowWindow(window, SW_SHOW);
  ShowWindow(window, SW_SHOW);
  record("ready", 0, 0, false);
  MSG msg;
  while (GetMessageW(&msg, nullptr, 0, 0) > 0) {
    TranslateMessage(&msg);
    DispatchMessageW(&msg);
  }
  return 0;
}
#else
#include <gtk/gtk.h>
gboolean draw(GtkWidget *, cairo_t *cr, gpointer) {
  cairo_set_source_rgb(cr, 1, 1, 1);
  cairo_paint(cr);
  cairo_set_source_rgb(cr, 0, 0, 0);
  cairo_set_font_size(cr, 15);
  cairo_move_to(cr, 20, 30);
  cairo_show_text(cr,
                  "DCU native drag fixture: drag blue square to green square");
  cairo_set_source_rgb(cr, .18, .43, .9);
  cairo_rectangle(cr, 80, 120, 120, 120);
  cairo_fill(cr);
  cairo_set_source_rgb(cr, .14, .7, .41);
  cairo_rectangle(cr, 480, 120, 120, 120);
  cairo_fill(cr);
  cairo_set_source_rgb(cr, 0, 0, 0);
  cairo_move_to(cr, 20, 320);
  auto text = "Completed drags: " + std::to_string(completed) +
              " | held: " + std::to_string(held) +
              " | moves: " + std::to_string(moves);
  cairo_show_text(cr, text.c_str());
  if (held) {
    cairo_move_to(cr, startX, startY);
    cairo_line_to(cr, pointerX, pointerY);
    cairo_stroke(cr);
  }
  return FALSE;
}
gboolean button(GtkWidget *widget, GdkEventButton *e, gpointer) {
  if (e->button == 1) {
    record(e->type == GDK_BUTTON_PRESS ? "down" : "up", e->x, e->y,
           e->type == GDK_BUTTON_PRESS);
    gtk_widget_queue_draw(widget);
  }
  return TRUE;
}
gboolean motion(GtkWidget *widget, GdkEventMotion *e, gpointer) {
  record("move", e->x, e->y, (e->state & GDK_BUTTON1_MASK) != 0);
  gtk_widget_queue_draw(widget);
  return TRUE;
}
int main(int argc, char **argv) {
  const std::string path = argc > 1 ? argv[1] : "drag-events.ndjson";
  logFile.open(path, std::ios::trunc);
  if (!logFile)
    return 1;
  gtk_init(&argc, &argv);
  auto window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
  gtk_window_set_title(GTK_WINDOW(window), "DCU native drag fixture");
  gtk_window_set_default_size(GTK_WINDOW(window), 740, 440);
  auto area = gtk_drawing_area_new();
  gtk_widget_add_events(area, GDK_BUTTON_PRESS_MASK | GDK_BUTTON_RELEASE_MASK |
                                  GDK_POINTER_MOTION_MASK);
  gtk_container_add(GTK_CONTAINER(window), area);
  g_signal_connect(area, "draw", G_CALLBACK(draw), nullptr);
  g_signal_connect(area, "button-press-event", G_CALLBACK(button), nullptr);
  g_signal_connect(area, "button-release-event", G_CALLBACK(button), nullptr);
  g_signal_connect(area, "motion-notify-event", G_CALLBACK(motion), nullptr);
  g_signal_connect(window, "destroy", G_CALLBACK(+[](GtkWidget *, gpointer) {
                     record("closed", pointerX, pointerY, held);
                     gtk_main_quit();
                   }),
                   nullptr);
  gtk_widget_show_all(window);
  record("ready", 0, 0, false);
  gtk_main();
  return 0;
}
#endif
