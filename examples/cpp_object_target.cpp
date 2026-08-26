#include <array>
#include <cstdint>
#include <cstdio>
#include <deque>
#include <functional>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <tuple>
#include <unordered_map>
#include <variant>
#include <vector>

#if defined(_MSC_VER)
#define FGDB_NOINLINE __declspec(noinline)
#else
#define FGDB_NOINLINE __attribute__((noinline))
#endif

namespace fixture {

class Shape {
public:
    virtual ~Shape() = default;
    [[nodiscard]] virtual double area() const = 0;
    [[nodiscard]] virtual std::string description() const = 0;
};

class Identified {
public:
    explicit Identified(std::uint64_t id) : id_(id) {}
    virtual ~Identified() = default;
    [[nodiscard]] std::uint64_t id() const { return id_; }

private:
    std::uint64_t id_;
};

class Rectangle final : public Shape, public Identified {
public:
    Rectangle(std::uint64_t id, double width, double height)
        : Identified(id), width_(width), height_(height) {}

    [[nodiscard]] double area() const override { return width_ * height_; }
    [[nodiscard]] std::string description() const override {
        return "Rectangle(" + std::to_string(width_) + ", " + std::to_string(height_) + ")";
    }

private:
    double width_;
    double height_;
};

class Circle final : public Shape, public Identified {
public:
    Circle(std::uint64_t id, double radius) : Identified(id), radius_(radius) {}

    [[nodiscard]] double area() const override { return 3.141592653589793 * radius_ * radius_; }
    [[nodiscard]] std::string description() const override {
        return "Circle(" + std::to_string(radius_) + ")";
    }

private:
    double radius_;
};

template <typename T>
struct TreeNode {
    T value;
    std::vector<std::unique_ptr<TreeNode<T>>> children;
    TreeNode<T>* parent = nullptr;

    explicit TreeNode(T initial) : value(std::move(initial)) {}

    TreeNode& append(T child_value) {
        auto child = std::make_unique<TreeNode>(std::move(child_value));
        child->parent = this;
        children.push_back(std::move(child));
        return *children.back();
    }
};

struct ErrorDetail {
    int code;
    std::string message;
    std::optional<std::string> source;
};

using Property = std::variant<std::int64_t, double, std::string, std::vector<std::uint8_t>>;

struct Scene {
    std::string name = "polymorphic test scene";
    std::vector<std::shared_ptr<Shape>> shapes;
    std::unordered_map<std::string, Property> properties;
    TreeNode<std::string> root{"root"};
    std::deque<std::tuple<std::uint64_t, std::string, bool>> events;
    std::weak_ptr<Shape> selected;
    Shape* raw_selected = nullptr;
    std::function<double(const Shape&)> evaluator;
    std::array<std::byte, 16> opaque{};
    std::optional<ErrorDetail> last_error;
};

FGDB_NOINLINE int overloaded(int value) {
    return value * 2;
}

FGDB_NOINLINE double overloaded(double value) {
    return value / 2.0;
}

FGDB_NOINLINE void throw_nested_error() {
    try {
        throw std::runtime_error("inner fixture failure");
    } catch (...) {
        std::throw_with_nested(std::logic_error("outer fixture failure"));
    }
}

FGDB_NOINLINE void cpp_objects_checkpoint(Scene& scene, Shape* selected, const ErrorDetail* error) {
    const auto summary = selected->description();
    const auto result = scene.evaluator(*selected) + overloaded(12) + overloaded(8.0);
    std::printf(
        "object checkpoint: %s area/result=%.2f error=%s\n",
        summary.c_str(),
        result,
        error == nullptr ? "none" : error->message.c_str()
    );
}

}  // namespace fixture

int main(void) {
    fixture::Scene scene;
    auto rectangle = std::make_shared<fixture::Rectangle>(1001, 12.5, 4.0);
    auto circle = std::make_shared<fixture::Circle>(1002, 3.25);
    scene.shapes = {rectangle, circle};
    scene.selected = circle;
    scene.raw_selected = circle.get();
    scene.properties.emplace("generation", std::int64_t{42});
    scene.properties.emplace("scale", 1.5);
    scene.properties.emplace("owner", std::string{"fgdb"});
    scene.properties.emplace("header", std::vector<std::uint8_t>{0xde, 0xad, 0xbe, 0xef});
    auto& branch = scene.root.append("branch");
    branch.append("leaf-a");
    branch.append("leaf-b");
    scene.events.emplace_back(1, "constructed", true);
    scene.events.emplace_back(2, "selected", false);
    scene.evaluator = [bias = 0.75](const fixture::Shape& shape) { return shape.area() + bias; };
    scene.last_error = fixture::ErrorDetail{404, "synthetic object error", "cpp-object-target"};

    fixture::cpp_objects_checkpoint(scene, scene.raw_selected, &*scene.last_error);

    try {
        fixture::throw_nested_error();
    } catch (const std::exception& error) {
        scene.last_error->message = error.what();
    }
    return scene.shapes.size() == 2 ? 0 : 1;
}
